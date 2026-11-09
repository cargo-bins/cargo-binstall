use std::{
    fmt, io,
    path::PathBuf,
    process::{ExitCode, ExitStatus},
};

use binstalk_fetchers::FetchError;
use binstalk_registry::RegistryError;
use compact_str::CompactString;
use exn::{ErrorExt, Exn, Frame, IteratorExt};
use miette::{Diagnostic, Report};
use ref_cast::RefCast;
use thiserror::Error;

use crate::bins;
#[allow(unused_imports)] // used in docs
use crate::helpers::cargo_toml::Error as CargoTomlError;

#[derive(Debug, Error)]
#[error("Invalid pkg-url {pkg_url} for {crate_name}@{version} on {target}: {reason}")]
pub struct InvalidPkgFmtError {
    pub crate_name: CompactString,
    pub version: CompactString,
    pub target: String,
    pub pkg_url: String,
    pub reason: &'static str,
}

/// Error kinds emitted by cargo-binstall.
///
/// Functions return [`Exn<BinstallError>`]: the root frame decides the exit
/// code and miette diagnostic code, while the underlying cause is attached as
/// a child frame. Use [`BinstallReport`] to render it with miette.
#[derive(Error, Diagnostic, Debug)]
#[non_exhaustive]
pub enum BinstallError {
    /// Internal: a task could not be joined.
    ///
    /// - Code: `binstall::internal::task_join`
    /// - Exit: 17
    #[error("failed to join task")]
    #[diagnostic(severity(error), code(binstall::internal::task_join))]
    TaskJoinError,

    /// The installation was cancelled by a user at a confirmation prompt,
    /// or user send a ctrl_c on all platforms or
    /// `SIGINT`, `SIGHUP`, `SIGTERM` or `SIGQUIT` on unix to the program.
    ///
    /// - Code: `binstall::user_abort`
    /// - Exit: 32
    #[error("installation cancelled by user")]
    #[diagnostic(severity(info), code(binstall::user_abort))]
    UserAbort,

    /// Package is signed but failed verification.
    ///
    /// - Code: `binstall::signature::invalid`
    /// - Exit: 40
    #[error("Crate {crate_name} is signed and package {package_name} failed verification")]
    #[diagnostic(severity(error), code(binstall::signature::invalid))]
    InvalidSignature {
        crate_name: CompactString,
        package_name: CompactString,
    },

    /// Package is not signed and policy requires it.
    ///
    /// - Code: `binstall::signature::missing`
    /// - Exit: 41
    #[error("Crate {0} does not have signing information")]
    #[diagnostic(severity(error), code(binstall::signature::missing))]
    MissingSignature(CompactString),

    /// A URL is invalid.
    ///
    /// This may be the result of a template in a Cargo manifest.
    ///
    /// - Code: `binstall::url_parse`
    /// - Exit: 65
    #[error("Failed to parse url")]
    #[diagnostic(severity(error), code(binstall::url_parse))]
    UrlParse,

    /// Failed to parse template.
    ///
    /// - Code: `binstall::template`
    /// - Exit: 67
    #[error("Failed to parse template")]
    #[diagnostic(severity(error), code(binstall::template))]
    TemplateParseError,

    /// Failed to fetch pre-built binaries.
    ///
    /// - Code: `binstall::fetch`
    /// - Exit: 68
    #[error("Failed to fetch pre-built binaries")]
    #[diagnostic(severity(error), code(binstall::fetch))]
    FetchError,

    /// Failed to download or failed to decode the body.
    ///
    /// - Code: `binstall::download`
    /// - Exit: 68
    #[error("Failed to download or decode the body")]
    #[diagnostic(severity(error), code(binstall::download))]
    Download,

    /// A subprocess failed.
    ///
    /// This is often about cargo-install calls.
    ///
    /// - Code: `binstall::subprocess`
    /// - Exit: 70
    #[error("subprocess {command} errored with {status}")]
    #[diagnostic(severity(error), code(binstall::subprocess))]
    SubProcess {
        command: Box<str>,
        status: ExitStatus,
    },

    /// A generic I/O error.
    ///
    /// - Code: `binstall::io`
    /// - Exit: 74
    #[error("I/O Error")]
    #[diagnostic(severity(error), code(binstall::io))]
    Io,

    /// Unknown registry name
    ///
    /// - Code: `binstall::cargo_registry`
    /// - Exit: 75
    #[error("Unknown registry name {0}, env `CARGO_REGISTRIES_{0}_INDEX` nor is it in .cargo/config.toml")]
    #[diagnostic(severity(error), code(binstall::cargo_registry))]
    UnknownRegistryName(CompactString),

    /// An error interacting with the crates.io API.
    ///
    /// This could either be a "not found" or a server/transport error.
    ///
    /// - Code: `binstall::cargo_registry`
    /// - Exit: 76
    #[error("Failed to fetch crate from registry")]
    #[diagnostic(severity(error), code(binstall::cargo_registry))]
    RegistryError,

    /// The override path to the cargo manifest is invalid or cannot be resolved.
    ///
    /// - Code: `binstall::cargo_manifest_path`
    /// - Exit: 77
    #[error("the --manifest-path is invalid or cannot be resolved")]
    #[diagnostic(severity(error), code(binstall::cargo_manifest_path))]
    CargoManifestPath,

    /// A parsing or validation error in a cargo manifest.
    ///
    /// This should be rare, as manifests are generally fetched from crates.io, which does its own
    /// validation upstream. The most common failure will therefore be for direct repository access
    /// and with the `--manifest-path` option.
    ///
    /// - Code: `binstall::cargo_manifest`
    /// - Exit: 78
    #[error("Failed to parse cargo manifest")]
    #[diagnostic(
        severity(error),
        code(binstall::cargo_manifest),
        help("If you used --manifest-path, check the Cargo.toml syntax.")
    )]
    CargoManifest,

    /// Failure to parse registry index url
    ///
    /// - Code: `binstall::cargo_registry`
    /// - Exit: 79
    #[error("Failed to parse registry index url")]
    #[diagnostic(severity(error), code(binstall::cargo_registry))]
    RegistryParseError,

    /// A version is not valid semver.
    ///
    /// Note that we use the [`semver`] crate, which parses Cargo version syntax; this may be
    /// somewhat stricter or very slightly different from other semver implementations.
    ///
    /// - Code: `binstall::version::parse`
    /// - Exit: 80
    #[error("version string '{0}' is not semver")]
    #[diagnostic(severity(error), code(binstall::version::parse))]
    VersionParse(CompactString),

    /// The crate@version syntax was used at the same time as the --version option.
    ///
    /// You can't do that as it's ambiguous which should apply.
    ///
    /// - Code: `binstall::conflict::version`
    /// - Exit: 84
    #[error("superfluous version specification")]
    #[diagnostic(
        severity(error),
        code(binstall::conflict::version),
        help("You cannot use both crate@version and the --version option. Remove one.")
    )]
    SuperfluousVersionOption,

    /// No binaries were found for the crate.
    ///
    /// When installing, either the binaries are specified in the crate's Cargo.toml, or they're
    /// inferred from the crate layout (e.g. src/main.rs or src/bins/name.rs). If no binaries are
    /// found through these methods, we can't know what to install!
    ///
    /// - Code: `binstall::resolve::binaries`
    /// - Exit: 86
    #[error("no binaries specified nor inferred")]
    #[diagnostic(
        severity(error),
        code(binstall::resolve::binaries),
        help("This crate doesn't specify any binaries, so there's nothing to install.")
    )]
    UnspecifiedBinaries,

    /// No viable targets were found.
    ///
    /// When installing, we attempt to find which targets the host (your computer) supports, and
    /// discover builds for these targets from the remote binary source. This error occurs when we
    /// fail to discover the host's target.
    ///
    /// You should in this case specify --target manually.
    ///
    /// - Code: `binstall::targets::none_host`
    /// - Exit: 87
    #[error("failed to discovered a viable target from the host")]
    #[diagnostic(
        severity(error),
        code(binstall::targets::none_host),
        help("Try to specify --target")
    )]
    NoViableTargets,

    /// Failed to find or install binaries.
    ///
    /// - Code: `binstall::bins`
    /// - Exit: 88
    #[error("failed to find or install binaries")]
    #[diagnostic(severity(error), code(binstall::bins))]
    BinFile,

    /// `Cargo.toml` of the crate does not have section "Package".
    ///
    /// - Code: `binstall::cargo_manifest`
    /// - Exit: 89
    #[error("Cargo.toml of crate {0} does not have section \"Package\"")]
    #[diagnostic(severity(error), code(binstall::cargo_manifest))]
    CargoTomlMissingPackage(CompactString),

    /// bin-dir configuration provided generates duplicate source path.
    ///
    /// - Code: `binstall::cargo_manifest`
    /// - Exit: 90
    #[error("bin-dir configuration provided generates duplicate source path: {path}")]
    #[diagnostic(severity(error), code(binstall::cargo_manifest))]
    DuplicateSourceFilePath { path: PathBuf },

    /// Fallback to `cargo-install` is disabled.
    ///
    /// - Code: `binstall::no_fallback_to_cargo_install`
    /// - Exit: 94
    #[error("Fallback to cargo-install is disabled")]
    #[diagnostic(severity(error), code(binstall::no_fallback_to_cargo_install))]
    NoFallbackToCargoInstall,

    /// The `pkg-url` or `pkg-fmt` is invalid.
    ///
    /// - Code: `binstall::invalid_pkg_fmt`
    /// - Exit: 95
    #[error(transparent)]
    #[diagnostic(severity(error), code(binstall::invalid_pkg_fmt))]
    InvalidPkgFmt(Box<InvalidPkgFmtError>),

    /// Request to GitHub API failed
    ///
    /// - Code: `binstall::gh_api_failure`
    /// - Exit: 96
    #[error("Request to GitHub API failed")]
    #[diagnostic(severity(error), code(binstall::gh_api_failure))]
    GhApiErr,

    /// Failed to parse target triple
    ///
    /// - Code: `binstall::target_triple_parse_error`
    /// - Exit: 97
    #[error("Failed to parse target triple")]
    #[diagnostic(severity(error), code(binstall::target_triple_parse_error))]
    TargetTripleParseError,

    /// Failed to shallow clone git repository
    ///
    /// - Code: `binstall::git`
    /// - Exit: 98
    #[cfg(feature = "git")]
    #[error("Failed to shallow clone git repository")]
    #[diagnostic(severity(error), code(binstall::git))]
    GitError,

    /// Failed to load manifest from workspace
    ///
    /// - Code: `binstall::load_manifest_from_workspace`
    /// - Exit: 99
    #[error("Failed to load manifest from workspace")]
    #[diagnostic(severity(error), code(binstall::load_manifest_from_workspace))]
    LoadManifestFromWSError,

    /// `cargo-install` does not support `--install-path`
    ///
    /// - Code: `binstall::cargo_install_does_not_support_install_path`
    /// - Exit: 100
    #[error("cargo-install does not support `--install-path`")]
    #[diagnostic(
        severity(error),
        code(binstall::cargo_install_does_not_support_install_path)
    )]
    CargoInstallDoesNotSupportInstallPath,

    /// Context of which crate the child error is about.
    #[error("For crate {0}")]
    CrateContext(CompactString),

    /// Parent of the failures of multiple crates when `--continue-on-failure` is specified.
    #[error("Failed to install {0} crates")]
    #[diagnostic(severity(error), code(binstall::many_failure))]
    Errors(usize),
}

impl BinstallError {
    /// Exit code of this kind, `None` for [`BinstallError::CrateContext`] and
    /// [`BinstallError::Errors`] which take the exit code of their children.
    fn exit_number(&self) -> Option<u8> {
        use BinstallError::*;
        let code: u8 = match self {
            TaskJoinError => 17,
            UserAbort => 32,
            InvalidSignature { .. } => 40,
            MissingSignature(_) => 41,
            UrlParse => 65,
            TemplateParseError => 67,
            FetchError => 68,
            Download => 68,
            SubProcess { .. } => 70,
            Io => 74,
            UnknownRegistryName(_) => 75,
            RegistryError => 76,
            CargoManifestPath => 77,
            CargoManifest => 78,
            RegistryParseError => 79,
            VersionParse(_) => 80,
            SuperfluousVersionOption => 84,
            UnspecifiedBinaries => 86,
            NoViableTargets => 87,
            BinFile => 88,
            CargoTomlMissingPackage(_) => 89,
            DuplicateSourceFilePath { .. } => 90,
            NoFallbackToCargoInstall => 94,
            InvalidPkgFmt(..) => 95,
            GhApiErr => 96,
            TargetTripleParseError => 97,
            #[cfg(feature = "git")]
            GitError => 98,
            LoadManifestFromWSError => 99,
            CargoInstallDoesNotSupportInstallPath => 100,
            CrateContext(_) | Errors(_) => return None,
        };

        // reserved codes
        debug_assert!(code != 64 && code != 16 && code != 1 && code != 2 && code != 0);

        Some(code)
    }

    /// Returns true if the installation was cancelled by the user, i.e. there
    /// is a [`BinstallError::UserAbort`] anywhere in `err`.
    pub fn is_user_abort(frame: &Frame) -> bool {
        find_map(frame, &mut |err| {
            matches!(err, BinstallError::UserAbort).then_some(())
        })
        .is_some()
    }

    /// Add crate context to `err`, unless it already has one.
    #[track_caller]
    pub fn crate_context(
        err: Exn<BinstallError>,
        crate_name: impl Into<CompactString>,
    ) -> Exn<BinstallError> {
        if matches!(&*err, Self::CrateContext(_)) {
            err
        } else {
            err.raise(Self::CrateContext(crate_name.into()))
        }
    }

    /// Combine errors of multiple crates, return `None` if `errors` is empty.
    #[track_caller]
    pub fn crate_errors(mut errors: Vec<Exn<BinstallError>>) -> Option<Exn<BinstallError>> {
        match errors.len() {
            0 => None,
            1 => errors.pop(),
            n => Some(errors.into_iter().raise(Self::Errors(n))),
        }
    }

    /// Recover the [`Exn<BinstallError>`] carried by `err` if any, otherwise
    /// raise `err` as a child of [`BinstallError::Io`].
    #[track_caller]
    pub fn from_io(err: io::Error) -> Exn<BinstallError> {
        match err.downcast::<IoCarrier>() {
            Ok(carrier) => carrier.0,
            Err(err) => err.raise().raise(Self::Io),
        }
    }

    /// Wrap `err` in an [`io::Error`], to be recovered by [`BinstallError::from_io`].
    pub fn into_io(err: Exn<BinstallError>) -> io::Error {
        io::Error::other(IoCarrier(err))
    }
}

/// Carries an [`Exn<BinstallError>`] through an [`io::Error`].
struct IoCarrier(Exn<BinstallError>);

impl fmt::Debug for IoCarrier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl fmt::Display for IoCarrier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for IoCarrier {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.frame().source()
    }
}

/// Find the first frame in `frame`'s tree (depth-first, pre-order) whose error
/// is a `T` and for which `f` returns `Some`.
fn find_map<'a, T: std::error::Error + 'static, U>(
    frame: &'a Frame,
    f: &mut impl FnMut(&'a T) -> Option<U>,
) -> Option<U> {
    frame
        .error()
        .downcast_ref::<T>()
        .and_then(&mut *f)
        .or_else(|| frame.children().iter().find_map(|child| find_map(child, f)))
}

/// Renders an [`Exn`] with miette and provides its exit code.
///
/// Every frame of the tree is rendered with the diagnostic code, help, url,
/// labels and source code of its own error, if that error is a known
/// [`Diagnostic`] (e.g. [`BinstallError`], [`FetchError`] or [`leon::ParseError`]).
///
/// Use `.map_err(BinstallReport::from)?` to propagate an [`Exn`] as a
/// [`miette::Report`].
pub struct BinstallReport(Exn);

impl<E: std::error::Error + Send + Sync + 'static> From<Exn<E>> for BinstallReport {
    fn from(err: Exn<E>) -> Self {
        Self(err.into())
    }
}

impl From<BinstallError> for BinstallReport {
    #[track_caller]
    fn from(err: BinstallError) -> Self {
        Self(err.into())
    }
}

impl BinstallReport {
    /// The recommended exit code for this error.
    ///
    /// It is the exit code of the first [`BinstallError`] in the tree that has
    /// one, i.e. the first crate that failed for [`BinstallError::Errors`].
    ///
    /// This will never output:
    /// - 0 (success)
    /// - 1 and 2 (catchall and shell)
    /// - 16 (binstall errors not handled here)
    /// - 64 (generic error)
    pub fn exit_code(&self) -> ExitCode {
        find_map(self.0.frame(), &mut BinstallError::exit_number)
            .unwrap_or(16)
            .into()
    }

    /// Returns true if the root error is [`BinstallError::UserAbort`], i.e.
    /// the installation was cancelled by the user and there is nothing else
    /// to report.
    pub fn is_user_abort(&self) -> bool {
        matches!(
            self.0.downcast_ref::<BinstallError>(),
            Some(BinstallError::UserAbort)
        )
    }

    /// Convert into a [`miette::Report`], `None` if the installation was
    /// cancelled by the user.
    pub fn into_report(self) -> Option<Report> {
        if self.is_user_abort() {
            None
        } else {
            Some(Report::new(self))
        }
    }

    fn root(&self) -> &FrameDiagnostic {
        FrameDiagnostic::ref_cast(self.0.frame())
    }
}

impl fmt::Debug for BinstallReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl fmt::Display for BinstallReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for BinstallReport {}

impl Diagnostic for BinstallReport {
    fn code<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        self.root().code()
    }

    fn severity(&self) -> Option<miette::Severity> {
        self.root().severity()
    }

    fn help<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        self.root().help()
    }

    fn url<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        self.root().url()
    }

    fn source_code(&self) -> Option<&dyn miette::SourceCode> {
        self.root().source_code()
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = miette::LabeledSpan> + '_>> {
        self.root().labels()
    }

    fn related<'a>(&'a self) -> Option<Box<dyn Iterator<Item = &'a dyn Diagnostic> + 'a>> {
        self.root().related()
    }

    fn diagnostic_source(&self) -> Option<&dyn Diagnostic> {
        self.root().diagnostic_source()
    }
}

/// A [`Diagnostic`] view of one [`Frame`].
///
/// A frame with a single child renders it as its cause, a frame with multiple
/// children (e.g. [`BinstallError::Errors`]) renders each as a related report.
#[derive(RefCast)]
#[repr(transparent)]
struct FrameDiagnostic(Frame);

impl FrameDiagnostic {
    /// The [`Diagnostic`] of this frame's own error, if it is a known one.
    fn own(&self) -> Option<&dyn Diagnostic> {
        macro_rules! downcast {
            ($($ty:ty),* $(,)?) => {
                $(
                    if let Some(err) = self.0.error().downcast_ref::<$ty>() {
                        return Some(err);
                    }
                )*
            };
        }

        downcast!(
            BinstallError,
            FetchError,
            RegistryError,
            bins::Error,
            leon::ParseError,
            leon::RenderError,
        );

        None
    }
}

impl FrameDiagnostic {
    /// Children of this frame, skipping a sole child which repeats the message
    /// of this frame, e.g. an `io::Error` source of an error which already
    /// includes it in its message.
    fn children(&self) -> &[Frame] {
        let mut children = self.0.children();
        while let [child] = children {
            if child.to_string() != self.0.to_string() {
                break;
            }
            children = child.children();
        }
        children
    }
}

impl fmt::Debug for FrameDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl fmt::Display for FrameDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for FrameDiagnostic {}

impl Diagnostic for FrameDiagnostic {
    fn code<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        self.own()?.code()
    }

    fn severity(&self) -> Option<miette::Severity> {
        self.own()?.severity()
    }

    fn help<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        self.own()?.help()
    }

    fn url<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        self.own()?.url()
    }

    fn source_code(&self) -> Option<&dyn miette::SourceCode> {
        self.own()?.source_code()
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = miette::LabeledSpan> + '_>> {
        self.own()?.labels()
    }

    fn diagnostic_source(&self) -> Option<&dyn Diagnostic> {
        match self.children() {
            [child] => Some(FrameDiagnostic::ref_cast(child)),
            _ => None,
        }
    }

    fn related<'a>(&'a self) -> Option<Box<dyn Iterator<Item = &'a dyn Diagnostic> + 'a>> {
        match self.children() {
            [] | [_] => None,
            children => {
                Some(Box::new(children.iter().map(|child| {
                    FrameDiagnostic::ref_cast(child) as &dyn Diagnostic
                })))
            }
        }
    }
}
