# Homebrew formula for a dedicated tap (indigoviolet/homebrew-tap).
#
# UNPUBLISHED. No lvu release, tag, tap repository or archive exists yet, so
# every url and sha256 below is a documented placeholder. Nothing here has been
# installed or audited by brew; this file records the intended shape so the
# first real release only has to substitute values.
#
# Before the first publication:
#   1. tag a release and upload the archives built by .github/workflows/release.yml
#   2. replace VERSION_PLACEHOLDER with the tag
#   3. replace each SHA256_PLACEHOLDER_* with the published checksum
#      (`shasum -a 256 lvu-<version>-<target>.tar.gz`)
#   4. add a LICENSE file to the repository; the crate declares
#      "MIT OR Apache-2.0" but no license text is committed yet
#   5. run `brew audit --strict --new lvu` and `brew test lvu`
#
# The archive is relocatable: bin/lvu resolves libexec/lvu/{python,bridge}
# relative to the real executable, so Homebrew's bin symlink works unchanged.
class Lvu < Formula
  desc "Live local log viewer for files, commands and stdin"
  homepage "https://github.com/indigoviolet/lvu"
  version "VERSION_PLACEHOLDER"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    on_arm do
      url "https://github.com/indigoviolet/lvu/releases/download/v#{version}/lvu-#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "SHA256_PLACEHOLDER_AARCH64_APPLE_DARWIN"
    end
    on_intel do
      url "https://github.com/indigoviolet/lvu/releases/download/v#{version}/lvu-#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "SHA256_PLACEHOLDER_X86_64_APPLE_DARWIN"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/indigoviolet/lvu/releases/download/v#{version}/lvu-#{version}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "SHA256_PLACEHOLDER_X86_64_UNKNOWN_LINUX_GNU"
    end
  end

  # Optional feature prerequisites, declared rather than vendored. The viewer
  # itself needs neither: capture, literal and /regex/ search, native filtering
  # and export work with no runtime dependency at all.
  depends_on "uv" => :optional  # advanced Polars filter/enrichment expressions
  depends_on "node" => :optional # 🧠 assistance bridge

  def install
    bin.install "bin/lvu"
    # resources.rs looks for <prefix>/libexec/lvu/<resource> relative to the
    # real executable, and Homebrew's `libexec` is <prefix>/libexec, so the
    # archive's directory is installed under it unchanged.
    libexec.install "libexec/lvu"
    doc.install Dir["share/doc/lvu/*"]
  end

  def caveats
    <<~EOS
      Optional features have their own prerequisites:
        advanced Polars expressions  uv (brew install uv)
        🧠 assistance                node (brew install node) and an
                                     authenticated agent CLI
      `lvu --resources` reports what resolved and what is missing.
    EOS
  end

  test do
    assert_match "Usage:", shell_output("#{bin}/lvu --help")
    # Proves the relocated payload is found through Homebrew's bin symlink.
    assert_match "installed beside the executable",
                 shell_output("#{bin}/lvu --resources")
  end
end
