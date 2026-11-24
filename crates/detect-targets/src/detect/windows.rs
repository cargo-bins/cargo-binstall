use std::mem;

use super::windows_targets::{Machine, alternative_targets};
use windows_sys::{
    Win32::{
        Foundation::{HANDLE, HMODULE, S_OK},
        System::{
            LibraryLoader::{GetProcAddress, LoadLibraryA},
            SystemInformation::{
                GetNativeSystemInfo, IMAGE_FILE_MACHINE, IMAGE_FILE_MACHINE_AMD64,
                IMAGE_FILE_MACHINE_ARM, IMAGE_FILE_MACHINE_ARM64, IMAGE_FILE_MACHINE_I386,
                PROCESSOR_ARCHITECTURE_AMD64, PROCESSOR_ARCHITECTURE_ARM64,
                PROCESSOR_ARCHITECTURE_INTEL,
            },
            Threading::{
                GetCurrentProcess, GetMachineTypeAttributes, IsWow64Process2, MACHINE_ATTRIBUTES,
                UserEnabled, Wow64Container,
            },
        },
    },
    core::BOOL,
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

type IsWow64Process2FuncType =
    unsafe extern "system" fn(HANDLE, *mut IMAGE_FILE_MACHINE, *mut IMAGE_FILE_MACHINE) -> BOOL;
const _: () = {
    // Same as for `GetMachineTypeAttributes` above.
    let _: IsWow64Process2FuncType = IsWow64Process2;
};

/// The machine that Windows itself runs on.
///
/// `GetNativeSystemInfo` reports x64 to x64 and Arm64EC processes on ARM64,
/// since both run under the x64 emulation, so it is only a fallback for
/// `IsWow64Process2`, which reports the real machine.
fn native_machine() -> Option<IMAGE_FILE_MACHINE> {
    // IsWow64Process2 is only available on Win10 1511+, so dynamically load it.
    let is_wow64_process2 = LibraryHandle::new(b"kernel32.dll\0").and_then(|kernel32| {
        // SAFETY: IsWow64Process2FuncType is checked to match the real function signature.
        unsafe { kernel32.get_proc_address::<IsWow64Process2FuncType>(b"IsWow64Process2\0") }
    });
    if let Some(is_wow64_process2) = is_wow64_process2 {
        let mut process_machine = 0;
        let mut native_machine = 0;
        // SAFETY: both pointers are valid for writes, and the pseudo handle
        // returned by GetCurrentProcess needs no closing.
        let ret = unsafe {
            is_wow64_process2(
                GetCurrentProcess(),
                &mut process_machine,
                &mut native_machine,
            )
        };
        if ret != 0 {
            return Some(native_machine);
        }
    }

    let mut system_info = mem::MaybeUninit::uninit();
    // SAFETY: GetNativeSystemInfo cannot fail and always fills in system_info.
    let system_info = unsafe {
        GetNativeSystemInfo(system_info.as_mut_ptr());
        system_info.assume_init()
    };
    // SAFETY: wProcessorArchitecture is the only member of the union that is
    // ever set.
    match unsafe { system_info.Anonymous.Anonymous.wProcessorArchitecture } {
        PROCESSOR_ARCHITECTURE_AMD64 => Some(IMAGE_FILE_MACHINE_AMD64),
        PROCESSOR_ARCHITECTURE_INTEL => Some(IMAGE_FILE_MACHINE_I386),
        PROCESSOR_ARCHITECTURE_ARM64 => Some(IMAGE_FILE_MACHINE_ARM64),
        _ => None,
    }
}

/// Guess the host target from the machine Windows runs on, rather than from
/// what this process is compiled for or emulated as.
///
/// This replaces `guess_host_triple`, which depends on winapi, and winapi
/// does not compile for Arm64EC.
pub(super) fn guess_host_triple() -> Option<&'static str> {
    match native_machine()? {
        IMAGE_FILE_MACHINE_AMD64 => Some("x86_64-pc-windows-msvc"),
        IMAGE_FILE_MACHINE_I386 => Some("i686-pc-windows-msvc"),
        IMAGE_FILE_MACHINE_ARM64 => Some("aarch64-pc-windows-msvc"),
        _ => None,
    }
}

pub(super) fn detect_alternative_targets(target: &str) -> impl Iterator<Item = String> + use<> {
    alternative_targets(target, |machine| {
        is_arch_supported(match machine {
            Machine::X64 => IMAGE_FILE_MACHINE_AMD64,
            Machine::X86 => IMAGE_FILE_MACHINE_I386,
            Machine::Arm32 => IMAGE_FILE_MACHINE_ARM,
            Machine::Arm64 => IMAGE_FILE_MACHINE_ARM64,
        })
    })
}
