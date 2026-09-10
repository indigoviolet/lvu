# lvu 9.9.9, rendered by packaging/homebrew/render-formula.sh from the
# release's SHA256SUMS. Edit packaging/homebrew/lvu.rb.in in the lvu repository
# and re-render; hand edits here are lost on the next release.
#
# No bottles: the archives are prebuilt executables, so there is nothing to
# compile. The archive is relocatable, so bin/lvu resolves libexec/lvu/* from
# its own canonicalized path and Homebrew's bin symlink works unchanged.
class Lvu < Formula
  desc "Live local log viewer for files, commands and stdin"
  homepage "https://github.com/indigoviolet/lvu"
  license any_of: ["MIT", "Apache-2.0"]

  # Exactly what the staged payload executes, and nothing more. `lvu
  # --resources` on a staged archive reports the two commands verbatim:
  #
  #   node dist/cli.js
  #   env PYTHONPATH=... uv run --no-project --python 3.12 \
  #       --with-requirements .../requirements.txt python -m lvu_expr_helper
  #
  # So the bridge needs node and the helper needs uv. The helper does not need
  # a system python: uv provisions CPython 3.12 itself. Both are optional
  # because the viewer needs neither -- capture, literal and /regex/ search,
  # native filtering, bookmarks and export work with no runtime dependency at
  # all, and a missing resource degrades one feature with a diagnostic instead
  # of failing to start.
  depends_on "node" => :optional
  depends_on "uv" => :optional

  on_macos do
    on_arm do
      # Built and executed on a native GitHub macos-15 runner (stage.sh
      # verification); the archive is unsigned and unnotarized, and human
      # terminal acceptance is pending. Homebrew installs from its own
      # download, so no Gatekeeper quarantine attribute is set.
      url "https://github.com/indigoviolet/lvu/releases/download/v9.9.9/lvu-9.9.9-aarch64-apple-darwin.tar.gz"
      sha256 "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
    end
    on_intel do
      # Retired from the supported set: render-formula.sh drops this block
      # unless --with-intel-darwin is passed to re-render a historical
      # release. The default formula never names this install target.
      # Also built on a macos-15 runner (cross-compiled or native) and untested.
      url "https://github.com/indigoviolet/lvu/releases/download/v9.9.9/lvu-9.9.9-x86_64-apple-darwin.tar.gz"
      sha256 "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"
    end
  end

  on_linux do
    # Both Linux archives are statically linked against musl, so they depend on
    # no system libc and no glibc version at all. A glibc build would inherit
    # the build runner's glibc and refuse to start on anything older. See
    # docs/distribution.md.
    on_arm do
      url "https://github.com/indigoviolet/lvu/releases/download/v9.9.9/lvu-9.9.9-aarch64-unknown-linux-musl.tar.gz"
      sha256 "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    end
    on_intel do
      url "https://github.com/indigoviolet/lvu/releases/download/v9.9.9/lvu-9.9.9-x86_64-unknown-linux-musl.tar.gz"
      sha256 "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    end
  end

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
      Optional features have their own prerequisites. They are declared rather
      than installed, because the viewer itself needs neither:
        advanced Polars expressions  uv    (brew install uv)
        Agent assistance             node  (brew install node) plus an
                                           installed, authenticated agent CLI
      Run `lvu --resources` to see what resolved and what is missing.
    EOS
  end

  test do
    assert_match "Usage: lvu [OPTIONS]", shell_output("#{bin}/lvu --help")

    # The payload must be found through Homebrew's bin symlink rather than from
    # any source checkout, which is the whole point of the relocatable layout.
    report = shell_output("#{bin}/lvu --resources")
    assert_match "origin: installed beside the executable", report
    assert_match "Python expression helper: found", report
    assert_match "agent bridge: found", report

    # An override is a pinned answer: naming an empty directory must report the
    # resource missing rather than silently falling back to the installation.
    pinned = testpath/"empty"
    pinned.mkpath
    pinned_report = shell_output("LVU_RESOURCE_ROOT=#{pinned} #{bin}/lvu --resources")
    assert_match "Python expression helper: missing", pinned_report
    assert_match "no other location was tried", pinned_report
  end
end
