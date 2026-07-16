# Homebrew formula — the CLI/daemon source-build install: sessiometer's headless,
# scripting channel, parallel to the `.app` GUI channel (the #171/#172 packaging set:
# #171 signs + notarizes + staples the .app, #172 is the Homebrew cask that installs it).
#
# It compiles the Rust crate at the repo root from source and installs the `sessiometer`
# binary (CLI + daemon). Locally compiled: NO notarization, NO code-signing — that is the
# GUI channel's concern (#171), deliberately kept off this channel's critical path.
#
# NEUTRALITY / DISCLAIMER (issue #273): sessiometer is unofficial and NOT affiliated with or
# endorsed by Anthropic. Distribution-facing metadata here stays neutral + factual — the `desc`
# below names "Claude Code" only nominatively (what the tool works with) and uses no third-party
# logos or marks. When the #172 cask lands it MUST carry the same "unofficial / not affiliated
# with or endorsed by Anthropic" disclaimer and an equally neutral, non-endorsing `desc` — the
# cask's user-facing metadata is a distribution surface too.
#
# HEAD-only for now. The crate is pre-release (version 0.1.0) with no tagged release, so
# there is no stable versioned source tarball to anchor `url`/`sha256` at yet. Install with:
#
#     brew install --HEAD --build-from-source ./Formula/sessiometer.rb
#
# STABLE-RELEASE TODO — when the first release is tagged (e.g. v0.1.0), add the stable `url`
# + `sha256` stanzas (see the commented block below) so `brew install sessiometer` works
# without `--HEAD`. Cutting/tagging that first release is an owner-gated decision and is out
# of scope for this formula.
class Sessiometer < Formula
  desc "Manage multiple Claude Code accounts and swap credentials before exhaustion"
  homepage "https://github.com/alexey-pelykh/sessiometer"

  # STABLE-RELEASE TODO (see the header comment). Uncomment and fill in when the first tag is
  # cut; `sha256` is the SHA-256 of the release tarball (`shasum -a 256 <tarball>`). These sit
  # here — before `license` — so the stanza order stays canonical once uncommented:
  #   url "https://github.com/alexey-pelykh/sessiometer/archive/refs/tags/vX.Y.Z.tar.gz"
  #   sha256 "0000000000000000000000000000000000000000000000000000000000000000"

  license "MIT"
  head "https://github.com/alexey-pelykh/sessiometer.git", branch: "main"

  depends_on :macos
  depends_on "rust" => :build

  def install
    # The crate is a single `[package]` at the repo root (ADR-0010) with a committed
    # Cargo.lock; `std_cargo_args` passes `--locked` for a reproducible source build and
    # installs the `sessiometer` binary into the formula prefix.
    system "cargo", "install", *std_cargo_args
  end

  test do
    # Hermetic, read-only checks — no daemon, keychain, or network needed.
    assert_match "sessiometer", shell_output("#{bin}/sessiometer --version")
    assert_match "manage multiple Claude Code accounts", shell_output("#{bin}/sessiometer --help")
    # `config path` resolves the effective config.toml location without reading its contents.
    assert_match "config.toml", shell_output("#{bin}/sessiometer config path")
  end
end
