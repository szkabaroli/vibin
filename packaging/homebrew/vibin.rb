# Reference copy of the Homebrew formula. The release workflow renders the
# real one (with the per-release version + sha256s filled in) and pushes it
# to szkabaroli/homebrew-tap, so users install with:
#
#   brew install szkabaroli/tap/vibin
#
# This file is here for review/history; the `sha256`/version fields below
# are placeholders that the workflow overwrites.
class Vibin < Formula
  desc "Terminal code editor with Claude Code sessions living next to your code"
  homepage "https://github.com/szkabaroli/vibin"
  version "0.0.0"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/szkabaroli/vibin/releases/download/v0.0.0/vibin-v0.0.0-aarch64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
    on_intel do
      url "https://github.com/szkabaroli/vibin/releases/download/v0.0.0/vibin-v0.0.0-x86_64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/szkabaroli/vibin/releases/download/v0.0.0/vibin-v0.0.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
    on_intel do
      url "https://github.com/szkabaroli/vibin/releases/download/v0.0.0/vibin-v0.0.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  def install
    bin.install "vibin"
    man1.install "vibin.1"
    bash_completion.install "completions/vibin.bash" => "vibin"
    zsh_completion.install "completions/_vibin"
    fish_completion.install "completions/vibin.fish"
  end

  test do
    assert_match "vibin #{version}", shell_output("#{bin}/vibin +version")
  end
end
