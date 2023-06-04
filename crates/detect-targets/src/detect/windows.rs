use std::mem;
use windows_dll::dll;
use windows_sys::{
    core::HRESULT,
    Win32::System::{
        SystemInformation::{
            IMAGE_FILE_MACHINE, IMAGE_FILE_MACHINE_AMD64, IMAGE_FILE_MACHINE_ARM,
            IMAGE_FILE_MACHINE_ARM64, IMAGE_FILE_MACHINE_I386,
        },
        Threading::{UserEnabled, Wow64Container, MACHINE_ATTRIBUTES},
    },
};

#[dll("Kernel32")]
extern "system" {
    #[allow(non_snake_case)]
    #[fallible]
    fn GetMachineTypeAttributes(
        machine: IMAGE_FILE_MACHINE,
        machine_attributes: *mut MACHINE_ATTRIBUTES,
    ) -> HRESULT;
}

fn is_arch_supported(arch: IMAGE_FILE_MACHINE) -> bool {
    let mut machine_attributes = mem::MaybeUninit::uninit();

    // SAFETY: GetMachineTypeAttributes takes type IMAGE_FILE_MACHINE
    // plus it takes a pointer to machine_attributes which is only
    // written to.
    match unsafe { GetMachineTypeAttributes(arch, machine_attributes.as_mut_ptr()) } {
        Ok(0) => {
            // SAFETY: Symbol GetMachineTypeAttributes exists and calls to it
            // succceeds.
            //
            // Thus, machine_attributes is initialized.
            let machine_attributes = unsafe { machine_attributes.assume_init() };

            (machine_attributes & (Wow64Container | UserEnabled)) != 0
        }
        _ => false,
    }
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
