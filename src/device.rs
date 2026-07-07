//! Shared Direct2D device for on-demand `SurfaceImageSource` drawing on the UI
//! thread, published through a reactor context so the chart and every card
//! sparkline render with the same device. Adapted from the windows-rs
//! `reactor/direct2d` sample, trimmed to the surface-image-source path.

use std::rc::Rc;
use std::sync::LazyLock;

use windows::Win32::Foundation::D2DERR_RECREATE_TARGET;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::core::{HRESULT, Interface, Result};
use windows_reactor::{Context, Updater};

/// The D3D11 device plus the Direct2D device. Single-threaded factory: all
/// drawing happens on the UI thread.
struct SharedDevice {
    _d3d_device: ID3D11Device,
    d2d_device: ID2D1Device,
}

impl SharedDevice {
    fn new() -> Result<Self> {
        let mut d3d_device: Option<ID3D11Device> = None;
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                None,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut d3d_device),
                None,
                None,
            )?;
        }
        let d3d_device = d3d_device.unwrap();

        let d2d_factory: ID2D1Factory1 =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };
        let dxgi_device: IDXGIDevice = d3d_device.cast()?;
        let d2d_device = unsafe { d2d_factory.CreateDevice(&dxgi_device)? };

        Ok(Self {
            _d3d_device: d3d_device,
            d2d_device,
        })
    }
}

/// Reference-counted device handle, usable as a context value and a `use_effect`
/// dependency. Equality is by identity, so a recreated device compares unequal
/// and drives device-keyed dependents to rebuild.
#[derive(Clone)]
pub struct Device(Rc<SharedDevice>);

impl Device {
    pub fn new() -> Result<Self> {
        Ok(Self(Rc::new(SharedDevice::new()?)))
    }

    pub fn d2d_device(&self) -> &ID2D1Device {
        &self.0.d2d_device
    }
}

impl PartialEq for Device {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

/// What the chart/cards need from the root: the shared device (`None` until
/// created) and a way to request recreation after device loss.
#[derive(Clone, PartialEq)]
pub struct Gpu {
    device: Option<Device>,
    recover: Updater<u32>,
}

impl Gpu {
    pub fn new(device: Option<Device>, recover: Updater<u32>) -> Self {
        Self { device, recover }
    }

    pub fn device(&self) -> Option<Device> {
        self.device.clone()
    }

    pub fn request_recovery(&self) {
        self.recover.call(|g| g.wrapping_add(1));
    }
}

static GPU_KEY: LazyLock<Context<()>> = LazyLock::new(|| Context::new(()));

/// The app-wide GPU context. `None` until the root installs it.
pub fn gpu_context() -> Context<Option<Gpu>> {
    Context {
        default: None,
        id: GPU_KEY.id,
    }
}

pub fn is_device_lost(hr: HRESULT) -> bool {
    matches!(
        hr,
        DXGI_ERROR_DEVICE_HUNG
            | DXGI_ERROR_DEVICE_REMOVED
            | DXGI_ERROR_DEVICE_RESET
            | DXGI_ERROR_DRIVER_INTERNAL_ERROR
            | DXGI_ERROR_INVALID_CALL
            | D2DERR_RECREATE_TARGET
    )
}
