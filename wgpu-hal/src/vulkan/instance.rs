use std::{
    cmp,
    ffi::{c_void, CStr, CString},
    mem, slice,
    sync::Arc,
    thread,
};

use ash::{
    extensions::{ext, khr},
    vk,
};

unsafe extern "system" fn debug_utils_messenger_callback(
    message_severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    message_type: vk::DebugUtilsMessageTypeFlagsEXT,
    callback_data_ptr: *const vk::DebugUtilsMessengerCallbackDataEXT,
    _user_data: *mut c_void,
) -> vk::Bool32 {
    use std::borrow::Cow;
    if thread::panicking() {
        return vk::FALSE;
    }

    let level = match message_severity {
        vk::DebugUtilsMessageSeverityFlagsEXT::ERROR => log::Level::Error,
        vk::DebugUtilsMessageSeverityFlagsEXT::WARNING => log::Level::Warn,
        vk::DebugUtilsMessageSeverityFlagsEXT::INFO => log::Level::Info,
        vk::DebugUtilsMessageSeverityFlagsEXT::VERBOSE => log::Level::Trace,
        _ => log::Level::Warn,
    };

    let cd = &*callback_data_ptr;

    let message_id_name = if cd.p_message_id_name.is_null() {
        Cow::from("")
    } else {
        CStr::from_ptr(cd.p_message_id_name).to_string_lossy()
    };
    let message = if cd.p_message.is_null() {
        Cow::from("")
    } else {
        CStr::from_ptr(cd.p_message).to_string_lossy()
    };

    log::log!(
        level,
        "{:?} [{} (0x{:x})]\n\t{}",
        message_type,
        message_id_name,
        cd.message_id_number,
        message,
    );

    if cd.queue_label_count != 0 {
        let labels = slice::from_raw_parts(cd.p_queue_labels, cd.queue_label_count as usize);
        let names = labels
            .iter()
            .flat_map(|dul_obj| {
                dul_obj
                    .p_label_name
                    .as_ref()
                    .map(|lbl| CStr::from_ptr(lbl).to_string_lossy())
            })
            .collect::<Vec<_>>();
        log::log!(level, "\tqueues: {}", names.join(", "));
    }

    if cd.cmd_buf_label_count != 0 {
        let labels = slice::from_raw_parts(cd.p_cmd_buf_labels, cd.cmd_buf_label_count as usize);
        let names = labels
            .iter()
            .flat_map(|dul_obj| {
                dul_obj
                    .p_label_name
                    .as_ref()
                    .map(|lbl| CStr::from_ptr(lbl).to_string_lossy())
            })
            .collect::<Vec<_>>();
        log::log!(level, "\tcommand buffers: {}", names.join(", "));
    }

    if cd.object_count != 0 {
        let labels = slice::from_raw_parts(cd.p_objects, cd.object_count as usize);
        //TODO: use color fields of `vk::DebugUtilsLabelExt`?
        let names = labels
            .iter()
            .map(|obj_info| {
                let name = obj_info
                    .p_object_name
                    .as_ref()
                    .map(|name| CStr::from_ptr(name).to_string_lossy())
                    .unwrap_or(Cow::Borrowed("?"));

                format!(
                    "(type: {:?}, hndl: 0x{:x}, name: {})",
                    obj_info.object_type, obj_info.object_handle, name
                )
            })
            .collect::<Vec<_>>();
        log::log!(level, "\tobjects: {}", names.join(", "));
    }

    vk::FALSE
}

impl super::Swapchain {
    unsafe fn release_resources(self, device: &ash::Device) -> Self {
        let _ = device.device_wait_idle();
        device.destroy_fence(self.fence, None);
        self
    }
}

impl super::Instance {
    pub fn required_extensions(
        entry: &ash::Entry,
        driver_api_version: u32,
        flags: crate::InstanceFlags,
    ) -> Result<Vec<&'static CStr>, crate::InstanceError> {
        let instance_extensions = entry
            .enumerate_instance_extension_properties()
            .map_err(|e| {
                log::info!("enumerate_instance_extension_properties: {:?}", e);
                crate::InstanceError
            })?;

        // Check our extensions against the available extensions
        let mut extensions: Vec<&'static CStr> = Vec::new();
        extensions.push(khr::Surface::name());

        // Platform-specific WSI extensions
        if cfg!(all(
            unix,
            not(target_os = "android"),
            not(target_os = "macos")
        )) {
            extensions.push(khr::XlibSurface::name());
            extensions.push(khr::XcbSurface::name());
            extensions.push(khr::WaylandSurface::name());
        }
        if cfg!(target_os = "android") {
            extensions.push(khr::AndroidSurface::name());
        }
        if cfg!(target_os = "windows") {
            extensions.push(khr::Win32Surface::name());
        }
        if cfg!(target_os = "macos") {
            extensions.push(ext::MetalSurface::name());
        }

        if flags.contains(crate::InstanceFlags::DEBUG) {
            extensions.push(ext::DebugUtils::name());
        }

        extensions.push(vk::KhrGetPhysicalDeviceProperties2Fn::name());

        // VK_KHR_storage_buffer_storage_class required for `Naga` on Vulkan 1.0 devices
        if driver_api_version == vk::API_VERSION_1_0 {
            extensions.push(vk::KhrStorageBufferStorageClassFn::name());
        }

        // Only keep available extensions.
        extensions.retain(|&ext| {
            if instance_extensions
                .iter()
                .any(|inst_ext| unsafe { CStr::from_ptr(inst_ext.extension_name.as_ptr()) == ext })
            {
                true
            } else {
                log::info!("Unable to find extension: {}", ext.to_string_lossy());
                false
            }
        });
        Ok(extensions)
    }

    /// # Safety
    ///
    /// - `raw_instance` must be created from `entry`
    /// - `raw_instance` must be created respecting `driver_api_version`, `extensions` and `flags`
    /// - `extensions` must be a superset of `required_extensions()` and must be created from the
    ///   same entry, driver_api_version and flags.
    pub unsafe fn from_raw(
        entry: ash::Entry,
        raw_instance: ash::Instance,
        driver_api_version: u32,
        extensions: Vec<&'static CStr>,
        flags: crate::InstanceFlags,
        drop_guard: super::DropGuard,
    ) -> Result<Self, crate::InstanceError> {
        if driver_api_version == vk::API_VERSION_1_0
            && !extensions.contains(&vk::KhrStorageBufferStorageClassFn::name())
        {
            log::warn!("Required VK_KHR_storage_buffer_storage_class extension is not supported");
            return Err(crate::InstanceError);
        }

        let debug_utils = if extensions.contains(&ext::DebugUtils::name()) {
            let extension = ext::DebugUtils::new(&entry, &raw_instance);
            let vk_info = vk::DebugUtilsMessengerCreateInfoEXT::builder()
                .flags(vk::DebugUtilsMessengerCreateFlagsEXT::empty())
                .message_severity(vk::DebugUtilsMessageSeverityFlagsEXT::all())
                .message_type(vk::DebugUtilsMessageTypeFlagsEXT::all())
                .pfn_user_callback(Some(debug_utils_messenger_callback));
            let messenger = extension
                .create_debug_utils_messenger(&vk_info, None)
                .unwrap();
            Some(super::DebugUtils {
                extension,
                messenger,
            })
        } else {
            None
        };

        let get_physical_device_properties = extensions
            .iter()
            .find(|&&ext| ext == vk::KhrGetPhysicalDeviceProperties2Fn::name())
            .map(|_| {
                vk::KhrGetPhysicalDeviceProperties2Fn::load(|name| {
                    mem::transmute(
                        entry.get_instance_proc_addr(raw_instance.handle(), name.as_ptr()),
                    )
                })
            });

        Ok(Self {
            shared: Arc::new(super::InstanceShared {
                raw: raw_instance,
                _drop_guard: drop_guard,
                flags,
                debug_utils,
                get_physical_device_properties,
                entry,
            }),
            extensions,
        })
    }

    #[allow(dead_code)]
    fn create_surface_from_xlib(
        &self,
        dpy: *mut vk::Display,
        window: vk::Window,
    ) -> super::Surface {
        if !self.extensions.contains(&khr::XlibSurface::name()) {
            panic!("Vulkan driver does not support VK_KHR_XLIB_SURFACE");
        }

        let surface = {
            let xlib_loader = khr::XlibSurface::new(&self.shared.entry, &self.shared.raw);
            let info = vk::XlibSurfaceCreateInfoKHR::builder()
                .flags(vk::XlibSurfaceCreateFlagsKHR::empty())
                .window(window)
                .dpy(dpy);

            unsafe { xlib_loader.create_xlib_surface(&info, None) }
                .expect("XlibSurface::create_xlib_surface() failed")
        };

        self.create_surface_from_vk_surface_khr(surface)
    }

    #[allow(dead_code)]
    fn create_surface_from_xcb(
        &self,
        connection: *mut vk::xcb_connection_t,
        window: vk::xcb_window_t,
    ) -> super::Surface {
        if !self.extensions.contains(&khr::XcbSurface::name()) {
            panic!("Vulkan driver does not support VK_KHR_XCB_SURFACE");
        }

        let surface = {
            let xcb_loader = khr::XcbSurface::new(&self.shared.entry, &self.shared.raw);
            let info = vk::XcbSurfaceCreateInfoKHR::builder()
                .flags(vk::XcbSurfaceCreateFlagsKHR::empty())
                .window(window)
                .connection(connection);

            unsafe { xcb_loader.create_xcb_surface(&info, None) }
                .expect("XcbSurface::create_xcb_surface() failed")
        };

        self.create_surface_from_vk_surface_khr(surface)
    }

    #[allow(dead_code)]
    fn create_surface_from_wayland(
        &self,
        display: *mut c_void,
        surface: *mut c_void,
    ) -> super::Surface {
        if !self.extensions.contains(&khr::WaylandSurface::name()) {
            panic!("Vulkan driver does not support VK_KHR_WAYLAND_SURFACE");
        }

        let surface = {
            let w_loader = khr::WaylandSurface::new(&self.shared.entry, &self.shared.raw);
            let info = vk::WaylandSurfaceCreateInfoKHR::builder()
                .flags(vk::WaylandSurfaceCreateFlagsKHR::empty())
                .display(display)
                .surface(surface);

            unsafe { w_loader.create_wayland_surface(&info, None) }.expect("WaylandSurface failed")
        };

        self.create_surface_from_vk_surface_khr(surface)
    }

    #[allow(dead_code)]
    fn create_surface_android(&self, window: *const c_void) -> super::Surface {
        let surface = {
            let a_loader = khr::AndroidSurface::new(&self.shared.entry, &self.shared.raw);
            let info = vk::AndroidSurfaceCreateInfoKHR::builder()
                .flags(vk::AndroidSurfaceCreateFlagsKHR::empty())
                .window(window as *mut _);

            unsafe { a_loader.create_android_surface(&info, None) }.expect("AndroidSurface failed")
        };

        self.create_surface_from_vk_surface_khr(surface)
    }

    #[allow(dead_code)]
    fn create_surface_from_hwnd(
        &self,
        hinstance: *mut c_void,
        hwnd: *mut c_void,
    ) -> super::Surface {
        if !self.extensions.contains(&khr::Win32Surface::name()) {
            panic!("Vulkan driver does not support VK_KHR_WIN32_SURFACE");
        }

        let surface = {
            let info = vk::Win32SurfaceCreateInfoKHR::builder()
                .flags(vk::Win32SurfaceCreateFlagsKHR::empty())
                .hinstance(hinstance)
                .hwnd(hwnd);
            let win32_loader = khr::Win32Surface::new(&self.shared.entry, &self.shared.raw);
            unsafe {
                win32_loader
                    .create_win32_surface(&info, None)
                    .expect("Unable to create Win32 surface")
            }
        };

        self.create_surface_from_vk_surface_khr(surface)
    }

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    fn create_surface_from_ns_view(&self, view: *mut c_void) -> super::Surface {
        use core_graphics_types::{base::CGFloat, geometry::CGRect};
        use objc::{
            class, msg_send,
            runtime::{Object, BOOL, YES},
            sel, sel_impl,
        };

        let layer = unsafe {
            let view = view as *mut Object;
            let existing: *mut Object = msg_send![view, layer];
            let class = class!(CAMetalLayer);

            let use_current = if existing.is_null() {
                false
            } else {
                let result: BOOL = msg_send![existing, isKindOfClass: class];
                result == YES
            };

            if use_current {
                existing
            } else {
                let layer: *mut Object = msg_send![class, new];
                let () = msg_send![view, setLayer: layer];
                let bounds: CGRect = msg_send![view, bounds];
                let () = msg_send![layer, setBounds: bounds];

                let window: *mut Object = msg_send![view, window];
                if !window.is_null() {
                    let scale_factor: CGFloat = msg_send![window, backingScaleFactor];
                    let () = msg_send![layer, setContentsScale: scale_factor];
                }
                layer
            }
        };

        let surface = {
            let metal_loader = ext::MetalSurface::new(&self.shared.entry, &self.shared.raw);
            let vk_info = vk::MetalSurfaceCreateInfoEXT::builder()
                .flags(vk::MetalSurfaceCreateFlagsEXT::empty())
                .layer(layer as *mut _)
                .build();

            unsafe { metal_loader.create_metal_surface(&vk_info, None).unwrap() }
        };

        self.create_surface_from_vk_surface_khr(surface)
    }

    fn create_surface_from_vk_surface_khr(&self, surface: vk::SurfaceKHR) -> super::Surface {
        let functor = khr::Surface::new(&self.shared.entry, &self.shared.raw);
        super::Surface {
            raw: surface,
            functor,
            instance: Arc::clone(&self.shared),
            swapchain: None,
        }
    }
}

impl Drop for super::InstanceShared {
    fn drop(&mut self) {
        unsafe {
            if let Some(du) = self.debug_utils.take() {
                du.extension
                    .destroy_debug_utils_messenger(du.messenger, None);
            }
            self.raw.destroy_instance(None);
        }
    }
}

impl crate::Instance<super::Api> for super::Instance {
    unsafe fn init(desc: &crate::InstanceDescriptor) -> Result<Self, crate::InstanceError> {
        let entry = match ash::Entry::new() {
            Ok(entry) => entry,
            Err(err) => {
                log::info!("Missing Vulkan entry points: {:?}", err);
                return Err(crate::InstanceError);
            }
        };
        let driver_api_version = match entry.try_enumerate_instance_version() {
            // Vulkan 1.1+
            Ok(Some(version)) => version,
            Ok(None) => vk::API_VERSION_1_0,
            Err(err) => {
                log::warn!("try_enumerate_instance_version: {:?}", err);
                return Err(crate::InstanceError);
            }
        };

        let app_name = CString::new(desc.name).unwrap();
        let app_info = vk::ApplicationInfo::builder()
            .application_name(app_name.as_c_str())
            .application_version(1)
            .engine_name(CStr::from_bytes_with_nul(b"wgpu-hal\0").unwrap())
            .engine_version(2)
            .api_version({
                // Pick the latest API version available, but don't go later than the SDK version used by `gfx_backend_vulkan`.
                cmp::min(driver_api_version, {
                    // This is the max Vulkan API version supported by `wgpu-hal`.
                    //
                    // If we want to increment this, there are some things that must be done first:
                    //  - Audit the behavioral differences between the previous and new API versions.
                    //  - Audit all extensions used by this backend:
                    //    - If any were promoted in the new API version and the behavior has changed, we must handle the new behavior in addition to the old behavior.
                    //    - If any were obsoleted in the new API version, we must implement a fallback for the new API version
                    //    - If any are non-KHR-vendored, we must ensure the new behavior is still correct (since backwards-compatibility is not guaranteed).
                    vk::HEADER_VERSION_COMPLETE
                })
            });

        let extensions = Self::required_extensions(&entry, driver_api_version, desc.flags)?;

        let instance_layers = entry.enumerate_instance_layer_properties().map_err(|e| {
            log::info!("enumerate_instance_layer_properties: {:?}", e);
            crate::InstanceError
        })?;

        // Check requested layers against the available layers
        let layers = {
            let mut layers: Vec<&'static CStr> = Vec::new();
            if desc.flags.contains(crate::InstanceFlags::VALIDATION) {
                layers.push(CStr::from_bytes_with_nul(b"VK_LAYER_KHRONOS_validation\0").unwrap());
            }

            // Only keep available layers.
            layers.retain(|&layer| {
                if instance_layers
                    .iter()
                    .any(|inst_layer| CStr::from_ptr(inst_layer.layer_name.as_ptr()) == layer)
                {
                    true
                } else {
                    log::warn!("Unable to find layer: {}", layer.to_string_lossy());
                    false
                }
            });
            layers
        };

        let vk_instance = {
            let str_pointers = layers
                .iter()
                .chain(extensions.iter())
                .map(|&s| {
                    // Safe because `layers` and `extensions` entries have static lifetime.
                    s.as_ptr()
                })
                .collect::<Vec<_>>();

            let create_info = vk::InstanceCreateInfo::builder()
                .flags(vk::InstanceCreateFlags::empty())
                .application_info(&app_info)
                .enabled_layer_names(&str_pointers[..layers.len()])
                .enabled_extension_names(&str_pointers[layers.len()..]);

            entry.create_instance(&create_info, None).map_err(|e| {
                log::warn!("create_instance: {:?}", e);
                crate::InstanceError
            })?
        };

        Self::from_raw(
            entry,
            vk_instance,
            driver_api_version,
            extensions,
            desc.flags,
            Box::new(()),
        )
    }

    unsafe fn create_surface(
        &self,
        has_handle: &impl raw_window_handle::HasRawWindowHandle,
    ) -> Result<super::Surface, crate::InstanceError> {
        use raw_window_handle::RawWindowHandle;

        match has_handle.raw_window_handle() {
            #[cfg(all(
                unix,
                not(target_os = "android"),
                not(target_os = "macos"),
                not(target_os = "ios"),
                not(target_os = "solaris")
            ))]
            RawWindowHandle::Wayland(handle)
                if self.extensions.contains(&khr::WaylandSurface::name()) =>
            {
                Ok(self.create_surface_from_wayland(handle.display, handle.surface))
            }
            #[cfg(all(
                unix,
                not(target_os = "android"),
                not(target_os = "macos"),
                not(target_os = "ios"),
                not(target_os = "solaris")
            ))]
            RawWindowHandle::Xlib(handle)
                if self.extensions.contains(&khr::XlibSurface::name()) =>
            {
                Ok(self.create_surface_from_xlib(handle.display as *mut _, handle.window))
            }
            #[cfg(all(
                unix,
                not(target_os = "android"),
                not(target_os = "macos"),
                not(target_os = "ios")
            ))]
            RawWindowHandle::Xcb(handle) if self.extensions.contains(&khr::XcbSurface::name()) => {
                Ok(self.create_surface_from_xcb(handle.connection, handle.window))
            }
            #[cfg(target_os = "android")]
            RawWindowHandle::Android(handle) => {
                Ok(self.create_surface_android(handle.a_native_window))
            }
            #[cfg(windows)]
            RawWindowHandle::Windows(handle) => {
                use winapi::um::libloaderapi::GetModuleHandleW;

                let hinstance = GetModuleHandleW(std::ptr::null());
                Ok(self.create_surface_from_hwnd(hinstance as *mut _, handle.hwnd))
            }
            #[cfg(target_os = "macos")]
            RawWindowHandle::MacOS(handle)
                if self.extensions.contains(&ext::MetalSurface::name()) =>
            {
                Ok(self.create_surface_from_ns_view(handle.ns_view))
            }
            _ => Err(crate::InstanceError),
        }
    }

    unsafe fn destroy_surface(&self, surface: super::Surface) {
        surface.functor.destroy_surface(surface.raw, None);
    }

    unsafe fn enumerate_adapters(&self) -> Vec<crate::ExposedAdapter<super::Api>> {
        let raw_devices = match self.shared.raw.enumerate_physical_devices() {
            Ok(devices) => devices,
            Err(err) => {
                log::error!("enumerate_adapters: {}", err);
                Vec::new()
            }
        };

        raw_devices
            .into_iter()
            .flat_map(|device| self.expose_adapter(device))
            .collect()
    }
}

impl crate::Surface<super::Api> for super::Surface {
    unsafe fn configure(
        &mut self,
        device: &super::Device,
        config: &crate::SurfaceConfiguration,
    ) -> Result<(), crate::SurfaceError> {
        let old = self
            .swapchain
            .take()
            .map(|sc| sc.release_resources(&device.shared.raw));

        let swapchain = device.create_swapchain(self, config, old)?;
        self.swapchain = Some(swapchain);

        Ok(())
    }

    unsafe fn unconfigure(&mut self, device: &super::Device) {
        if let Some(sc) = self.swapchain.take() {
            let swapchain = sc.release_resources(&device.shared.raw);
            swapchain.functor.destroy_swapchain(swapchain.raw, None);
        }
    }

    unsafe fn acquire_texture(
        &mut self,
        timeout_ms: u32,
    ) -> Result<Option<crate::AcquiredSurfaceTexture<super::Api>>, crate::SurfaceError> {
        let sc = self.swapchain.as_mut().unwrap();
        let timeout_ns = timeout_ms as u64 * super::MILLIS_TO_NANOS;

        // will block if no image is available
        let (index, suboptimal) =
            match sc
                .functor
                .acquire_next_image(sc.raw, timeout_ns, vk::Semaphore::null(), sc.fence)
            {
                Ok(pair) => pair,
                Err(error) => {
                    return match error {
                        vk::Result::TIMEOUT => Ok(None),
                        vk::Result::NOT_READY | vk::Result::ERROR_OUT_OF_DATE_KHR => {
                            Err(crate::SurfaceError::Outdated)
                        }
                        vk::Result::ERROR_SURFACE_LOST_KHR => Err(crate::SurfaceError::Lost),
                        other => Err(crate::DeviceError::from(other).into()),
                    }
                }
            };

        // special case for Intel Vulkan returning bizzare values (ugh)
        if sc.device.vendor_id == crate::auxil::db::intel::VENDOR && index > 0x100 {
            return Err(crate::SurfaceError::Outdated);
        }

        let fences = &[sc.fence];

        sc.device
            .raw
            .wait_for_fences(fences, true, !0)
            .map_err(crate::DeviceError::from)?;
        sc.device
            .raw
            .reset_fences(fences)
            .map_err(crate::DeviceError::from)?;

        let texture = super::SurfaceTexture {
            index,
            texture: super::Texture {
                raw: sc.images[index as usize],
                drop_guard: None,
                block: None,
                usage: sc.config.usage,
                aspects: crate::FormatAspects::COLOR,
                format_info: sc.config.format.describe(),
                raw_flags: vk::ImageCreateFlags::empty(),
            },
        };
        Ok(Some(crate::AcquiredSurfaceTexture {
            texture,
            suboptimal,
        }))
    }

    unsafe fn discard_texture(&mut self, _texture: super::SurfaceTexture) {}
}
