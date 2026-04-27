class CodexResponsesAdapter < Formula
  desc "Translate OpenAI Responses API to Chat Completions API"
  homepage "https://github.com/szj2ys/codex-responses-adapter"
  version "VERSION_NUM"
  license "MIT"

  on_macos do
    on_intel do
      url "https://github.com/szj2ys/codex-responses-adapter/releases/download/VERSION/codex-responses-adapter-VERSION-x86_64-apple-darwin.tar.gz"
      sha256 "SHA_MAC_INTEL"
    end
    on_arm do
      url "https://github.com/szj2ys/codex-responses-adapter/releases/download/VERSION/codex-responses-adapter-VERSION-aarch64-apple-darwin.tar.gz"
      sha256 "SHA_MAC_ARM"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/szj2ys/codex-responses-adapter/releases/download/VERSION/codex-responses-adapter-VERSION-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "SHA_LINUX_INTEL"
    end
    on_arm do
      url "https://github.com/szj2ys/codex-responses-adapter/releases/download/VERSION/codex-responses-adapter-VERSION-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "SHA_LINUX_ARM"
    end
  end

  def install
    bin.install "codex-responses-adapter"
  end

  test do
    system "#{bin}/codex-responses-adapter", "--help"
  end
end
