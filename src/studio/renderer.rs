//! Which eframe renderer the studio windows use.
//!
//! glow (OpenGL) is the default: it is what the transparent, acrylic window is tuned on. A
//! Windows machine without a vendor GPU driver (a VM, an RDP session, the Basic Display
//! Adapter) only offers the OpenGL 1.1 software implementation, and glow needs 2.0+, so the
//! studio exits before it draws anything. winit cannot build a second event loop in one
//! process, so the fallback is to start the studio again with `NOREN_STUDIO_RENDERER=wgpu`;
//! wgpu reaches Direct3D 12, which falls back to the WARP software adapter.

use std::process::{Command, ExitCode};

use eframe::Renderer;

/// Environment variable that forces a renderer: `glow` or `wgpu`.
pub(crate) const ENV: &str = "NOREN_STUDIO_RENDERER";

/// The renderer to start with, from the value of [`ENV`].
pub(crate) fn chosen(env: Option<&str>) -> Renderer {
    match env {
        Some("wgpu") => Renderer::Wgpu,
        _ => Renderer::Glow,
    }
}

/// The renderer to retry with after a failed start, if any. Only an OpenGL failure is worth a
/// retry, and only once: a forced renderer is never overridden.
pub(crate) fn retry(gl_failed: bool, env: Option<&str>) -> Option<&'static str> {
    (gl_failed && env.is_none()).then_some("wgpu")
}

/// True when the error came from glutin or glow, i.e. OpenGL itself was not usable.
fn gl_failed(e: &eframe::Error) -> bool {
    matches!(
        e,
        eframe::Error::Glutin(_) | eframe::Error::NoGlutinConfigs(..) | eframe::Error::OpenGL(_)
    )
}

/// Turns the result of `eframe::run_native` into an exit code, starting this program again
/// with wgpu when OpenGL was not usable.
pub(crate) fn finish(result: eframe::Result) -> ExitCode {
    let Err(e) = result else {
        return ExitCode::SUCCESS;
    };
    let env = std::env::var(ENV).ok();
    let Some(next) = retry(gl_failed(&e), env.as_deref()) else {
        eprintln!("noren-studio: {e}");
        return ExitCode::FAILURE;
    };
    eprintln!("noren-studio: {e}; starting again with {ENV}={next}");
    let status = std::env::current_exe().and_then(|exe| {
        Command::new(exe)
            .args(std::env::args_os().skip(1))
            .env(ENV, next)
            .status()
    });
    match status {
        Ok(s) if s.success() => ExitCode::SUCCESS,
        Ok(s) => ExitCode::from(s.code().and_then(|c| u8::try_from(c).ok()).unwrap_or(1)),
        Err(err) => {
            eprintln!("noren-studio: could not start again: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glow_by_default_and_wgpu_when_forced() {
        assert_eq!(chosen(None), Renderer::Glow);
        assert_eq!(chosen(Some("glow")), Renderer::Glow);
        assert_eq!(chosen(Some("wgpu")), Renderer::Wgpu);
    }

    #[test]
    fn an_opengl_failure_retries_once_with_wgpu() {
        // Win11 without a GPU driver: "egui_glow requires opengl 2.0+" and the studio never opened.
        assert_eq!(retry(true, None), Some("wgpu"));
        assert_eq!(retry(true, Some("wgpu")), None, "the retry itself failed: stop");
        assert_eq!(retry(true, Some("glow")), None, "glow was forced: respect it");
        assert_eq!(retry(false, None), None, "not an OpenGL problem");
    }

    #[test]
    fn app_errors_are_not_opengl_failures() {
        let e = eframe::Error::AppCreation("boom".into());
        assert!(!gl_failed(&e));
    }
}
