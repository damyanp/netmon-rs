//! Shared canvas GPU device for on-demand `CanvasImageSource` drawing on the UI
//! thread, published through a reactor context so the chart and every card
//! sparkline render with the same device.

use std::rc::Rc;
use std::sync::LazyLock;

use windows::core::Result;
use windows_canvas::GpuDevice;
use windows_reactor::{Context, Updater};

/// Reference-counted device handle, usable as a context value and a `use_effect`
/// dependency. Equality is by identity, so a recreated device compares unequal
/// and drives device-keyed dependents to rebuild.
#[derive(Clone)]
pub struct Device(Rc<GpuDevice>);

impl Device {
    pub fn new() -> Result<Self> {
        Ok(Self(Rc::new(GpuDevice::new()?)))
    }

    pub fn gpu_device(&self) -> &GpuDevice {
        &self.0
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
