# Source of truth for the cask in ahmedash95/homebrew-tap (Casks/toma.rb).
# Release CI bumps version + sha256 there when HOMEBREW_TAP_TOKEN is set.
cask "toma" do
  version "0.1.0"
  sha256 "REPLACE_ON_FIRST_RELEASE"

  url "https://github.com/ahmedash95/toma/releases/download/v#{version}/Toma-#{version}.dmg"
  name "Toma"
  desc "Native macOS workspace for coordinating coding agents"
  homepage "https://github.com/ahmedash95/toma"

  depends_on arch: :arm64
  depends_on macos: ">= :ventura"

  app "Toma.app"

  # Ad-hoc signed, not notarized — strip quarantine so Gatekeeper allows launch.
  postflight do
    system_command "/usr/bin/xattr",
                   args:         ["-dr", "com.apple.quarantine", "#{appdir}/Toma.app"],
                   must_succeed: false
  end

  uninstall quit: "dev.toma.app"
end
