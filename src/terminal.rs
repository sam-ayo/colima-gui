//! Opens a shell in a container in the Terminal app.

use std::process::Command;

use anyhow::{Result, bail};

/// Opens a Terminal window that runs `docker exec` for the container. The
/// shell is bash if the image has it, else sh.
pub fn open_shell(id: &str, docker_host: &str) -> Result<()> {
    let shell = "if command -v bash >/dev/null; then exec bash; else exec sh; fi";
    let command = format!(
        "DOCKER_HOST={} docker exec -it {} sh -c {}",
        sh_quote(docker_host),
        sh_quote(id),
        sh_quote(shell)
    );
    let script = format!(
        "tell application \"Terminal\"\nactivate\ndo script \"{}\"\nend tell",
        command.replace('\\', "\\\\").replace('"', "\\\"")
    );
    let out = Command::new("osascript").args(["-e", &script]).output()?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("could not open Terminal: {}", err.trim());
    }
    Ok(())
}

/// Quotes a value for a POSIX shell.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::sh_quote;

    #[test]
    fn quotes_for_the_shell() {
        assert_eq!(sh_quote("abc"), "'abc'");
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
    }
}
