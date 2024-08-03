use std::mem;
use windows_sys::Win32::{
    Foundation::{HMODULE, S_OK},
    System::{
        LibraryLoader::{GetProcAddress, LoadLibraryA},
        SystemInformation::{
            IMAGE_FILE_MACHINE, IMAGE_FILE_MACHINE_AMD64, IMAGE_FILE_MACHINE_ARM,
            IMAGE_FILE_MACHINE_ARM64, IMAGE_FILE_MACHINE_I386,
        },
        Threading::{GetMachineTypeAttributes, UserEnabled, Wow64Container, MACHINE_ATTRIBUTES},
    },
};

struct LibraryHandle(HMODULE);

impl LibraryHandle {
    fn new(name: &[u8]) -> Option<Self> {
        let handle = unsafe { LoadLibraryA(name.as_ptr() as _) };
        (!handle.is_null()).then_some(Self(handle))
    }

    /// Get a function pointer to a function in the library.
    /// # SAFETY
    ///
    /// The caller must ensure that the function signature matches the actual function.
    /// The easiest way to do this is to add an entry to windows_sys_no_link.list and use the
    /// generated function for `func_signature`.
    ///
    /// The function returned cannot be used after the handle is dropped.
    unsafe fn get_proc_address<F>(&self, name: &[u8]) -> Option<F> {
        let symbol = unsafe { GetProcAddress(self.0, name.as_ptr() as _) };
        symbol.map(|symbol| unsafe { mem::transmute_copy(&symbol) })
    }
}

type GetMachineTypeAttributesFuncType =
    unsafe extern "system" fn(u16, *mut MACHINE_ATTRIBUTES) -> i32;
const _: () = {
    // Ensure that our hand-written signature matches the actual function signature.
    // We can't use `GetMachineTypeAttributes` outside of a const scope otherwise we'll end up statically linking to
    // it, which will fail to load on older versions of Windows.
    let _: GetMachineTypeAttributesFuncType = GetMachineTypeAttributes;
};

fn is_arch_supported_inner(arch: IMAGE_FILE_MACHINE) -> Option<bool> {
    // GetMachineTypeAttributes is only available on Win11 22000+, so dynamically load it.
    let kernel32 = LibraryHandle::new(b"kernel32.dll\0")?;
    // SAFETY: GetMachineTypeAttributesFuncType is checked to match the real function signature.
    let get_machine_type_attributes = unsafe {
        kernel32.get_proc_address::<GetMachineTypeAttributesFuncType>(b"GetMachineTypeAttributes\0")
    }?;

    let mut machine_attributes = mem::MaybeUninit::uninit();
    if unsafe { get_machine_type_attributes(arch, machine_attributes.as_mut_ptr()) } == S_OK {
        let machine_attributes = unsafe { machine_attributes.assume_init() };
        Some((machine_attributes & (Wow64Container | UserEnabled)) != 0)
    } else {
        Some(false)
    }
}

fn is_arch_supported(arch: IMAGE_FILE_MACHINE) -> bool {
    is_arch_supported_inner(arch).unwrap_or(false)
}

pub(super) fn detect_alternative_targets(target: &str) -> impl Iterator<Item = String> {
    let (prefix, abi) = target
        .rsplit_once('-')
        .expect("unwrap: target always has a -");

    let arch = prefix
        .split_once('-')
        .expect("unwrap: target always has at least two -")
        .0;

    let msvc_fallback_target = (abi != "msvc").then(|| format!("{prefix}-msvc"));

    let gnu_fallback_targets = (abi == "msvc")
        .then(|| [format!("{prefix}-gnu"), format!("{prefix}-gnullvm")])
        .into_iter()
        .flatten();

    let x64_fallback_targets = (arch != "x86_64" && is_arch_supported(IMAGE_FILE_MACHINE_AMD64))
        .then_some([
            "x86_64-pc-windows-msvc",
            "x86_64-pc-windows-gnu",
            "x86_64-pc-windows-gnullvm",
        ])
        .into_iter()
        .flatten()
        .map(ToString::to_string);

    let x86_fallback_targets = (arch != "x86" && is_arch_supported(IMAGE_FILE_MACHINE_I386))
        .then_some([
            "i586-pc-windows-msvc",
            "i586-pc-windows-gnu",
            "i586-pc-windows-gnullvm",
            "i686-pc-windows-msvc",
            "i686-pc-windows-gnu",
            "i686-pc-windows-gnullvm",
        ])
        .into_iter()
        .flatten()
        .map(ToString::to_string);

    let arm32_fallback_targets = (arch != "thumbv7a" && is_arch_supported(IMAGE_FILE_MACHINE_ARM))
        .then_some([
            "thumbv7a-pc-windows-msvc",
            "thumbv7a-pc-windows-gnu",
            "thumbv7a-pc-windows-gnullvm",
        ])
        .into_iter()
        .flatten()
        .map(ToString::to_string);

    let arm64_fallback_targets = (arch != "aarch64" && is_arch_supported(IMAGE_FILE_MACHINE_ARM64))
        .then_some([
            "aarch64-pc-windows-msvc",
            "aarch64-pc-windows-gnu",
            "aarch64-pc-windows-gnullvm",
        ])
        .into_iter()
        .flatten()
        .map(ToString::to_string);

    msvc_fallback_target
        .into_iter()
        .chain(gnu_fallback_targets)
        .chain(x64_fallback_targets)
        .chain(x86_fallback_targets)
        .chain(arm32_fallback_targets)
        .chain(arm64_fallback_targets)
}
