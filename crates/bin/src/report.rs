//! Render an [`Error`] tree with miette.

use std::{error::Error as StdError, fmt};

use binstalk::errors::{Error, Exit, Frame, Message};
use miette::{Diagnostic, LabeledSpan, Severity, SourceCode};
use ref_cast::RefCast;

/// Renders an [`Error`] with miette, every frame of the tree becomes a
/// [`Diagnostic`].
pub struct Report(pub Error);

impl Report {
    fn root(&self) -> &FrameDiagnostic {
        FrameDiagnostic::ref_cast(self.0.frame())
    }
}

impl fmt::Debug for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.root(), f)
    }
}

impl StdError for Report {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.root().source()
    }
}

impl Diagnostic for Report {
    fn code<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        self.root().code()
    }

    fn severity(&self) -> Option<Severity> {
        self.root().severity()
    }

    fn help<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        self.root().help()
    }

    fn url<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        self.root().url()
    }

    fn source_code(&self) -> Option<&dyn SourceCode> {
        self.root().source_code()
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = LabeledSpan> + '_>> {
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
/// - An [`Exit`] frame provides the diagnostic code, and is merged with its
///   sole child if it is a [`Message`] without children, e.g. one created by
///   [`Exit::with`], to avoid printing two lines for one error.
/// - A [`Message`] provides the help.
/// - [`leon`] errors provide their own diagnostic, e.g. labels in the
///   template.
/// - A frame with a single child renders it as its cause, a frame with
///   multiple children renders each as a related report.
#[derive(RefCast)]
#[repr(transparent)]
struct FrameDiagnostic(Frame);

impl FrameDiagnostic {
    fn exit(&self) -> Option<Exit> {
        self.0.error().downcast_ref::<Exit>().copied()
    }

    /// The frame whose error describes this frame.
    fn describing(&self) -> &Frame {
        if self.exit().is_some() {
            if let [child] = self.0.children() {
                if child.error().is::<Message>() && child.children().is_empty() {
                    return child;
                }
            }
        }
        &self.0
    }

    fn message(&self) -> Option<&Message> {
        self.describing().error().downcast_ref::<Message>()
    }

    /// The [`Diagnostic`] of this frame's own error, if it is a known one.
    fn own(&self) -> Option<&dyn Diagnostic> {
        let err = self.0.error();
        if let Some(err) = err.downcast_ref::<leon::ParseError>() {
            Some(err)
        } else if let Some(err) = err.downcast_ref::<leon::RenderError>() {
            Some(err)
        } else {
            None
        }
    }

    /// Children of this frame, skipping a sole child which repeats the message
    /// of this frame, e.g. an `io::Error` source of an error which already
    /// includes it in its message.
    fn children(&self) -> &[Frame] {
        let describing = self.describing();
        let mut children = describing.children();
        while let [child] = children {
            if child.error().to_string() != describing.error().to_string() {
                break;
            }
            children = child.children();
        }
        children
    }
}

impl FrameDiagnostic {
    fn sole_child(&self) -> Option<&Self> {
        match self.children() {
            [child] => Some(Self::ref_cast(child)),
            _ => None,
        }
    }

    /// Returns true if this frame has something other than its message to
    /// show.
    fn is_rich(&self) -> bool {
        self.code().is_some()
            || self.help().is_some()
            || self.labels().is_some()
            || self.children().len() > 1
    }

    /// Returns true if this frame, or any frame in its chain of sole children,
    /// is [rich](Self::is_rich).
    fn is_chain_rich(&self) -> bool {
        self.is_rich() || self.sole_child().is_some_and(Self::is_chain_rich)
    }
}

impl fmt::Debug for FrameDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl fmt::Display for FrameDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.describing().error(), f)
    }
}

impl StdError for FrameDiagnostic {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.sole_child().map(|child| child as &dyn StdError)
    }
}

impl Diagnostic for FrameDiagnostic {
    fn code<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        match self.exit() {
            Some(exit) => Some(Box::new(exit.diagnostic_code())),
            None => self.own()?.code(),
        }
    }

    fn severity(&self) -> Option<Severity> {
        self.own()?.severity()
    }

    fn help<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        if let Some(help) = self.message().and_then(Message::help) {
            Some(Box::new(help))
        } else if let Some(help) = self.exit().and_then(Exit::help) {
            Some(Box::new(help))
        } else {
            self.own()?.help()
        }
    }

    fn url<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        self.own()?.url()
    }

    fn source_code(&self) -> Option<&dyn SourceCode> {
        self.own()?.source_code()
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = LabeledSpan> + '_>> {
        self.own()?.labels()
    }

    fn diagnostic_source(&self) -> Option<&dyn Diagnostic> {
        // Only render the cause as a nested report if there is something
        // other than the message to show in the rest of the chain, otherwise
        // let miette render it as plain lines via `source()`.
        self.sole_child()
            .filter(|child| child.is_chain_rich())
            .map(|child| child as &dyn Diagnostic)
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

#[cfg(test)]
mod tests {
    use binstalk::errors::{message, ErrorExt, ResultExt};
    use miette::{GraphicalReportHandler, GraphicalTheme};

    use super::*;

    fn render(err: Error) -> String {
        let mut out = String::new();
        GraphicalReportHandler::new_themed(GraphicalTheme::unicode_nocolor())
            .with_width(200)
            .render_report(&mut out, &Report(err))
            .unwrap();
        out
    }

    #[test]
    fn exit_merged_with_message() {
        let out = render(Exit::SubProcess.with(message!("subprocess cargo errored")));
        assert!(out.contains("binstall::subprocess"), "{out}");
        assert!(out.contains("subprocess cargo errored"), "{out}");
        assert!(!out.contains("subprocess failed"), "{out}");
    }

    #[test]
    fn help_and_plain_chain() {
        let err: Error = std::io::Error::other("os error")
            .raise()
            .raise(message!("Failed to open").with_help("Check the path"))
            .into();
        let out = render(Err::<(), _>(err).or_raise_err(Exit::Io).unwrap_err());
        assert!(out.contains("binstall::io"), "{out}");
        assert!(out.contains("help: Check the path"), "{out}");
        assert!(out.contains("os error"), "{out}");
    }

    #[test]
    fn leon_labels() {
        let err = leon::Template::parse("{ broken").unwrap_err();
        let out = render(
            Err::<(), _>(err)
                .or_raise_err(Exit::TemplateParse)
                .unwrap_err(),
        );
        assert!(out.contains("binstall::template"), "{out}");
        assert!(out.contains("{ broken"), "{out}");
    }
}
