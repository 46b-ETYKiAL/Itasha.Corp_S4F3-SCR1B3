# Homebrew cask for SCR1B3. Install: brew install --cask scr1b3
# (self-update is disabled in cask installs; `brew upgrade` owns updates.)
cask "scr1b3" do
  version "0.1.0"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"

  url "https://github.com/46b-ETYKiAL/Itasha.Corp_S4F3-SCR1B3/releases/download/v#{version}/scr1b3-aarch64-apple-darwin.dmg"
  name "SCR1B3"
  desc "Fast, telemetry-free, cross-platform code/text editor"
  homepage "https://github.com/46b-ETYKiAL/Itasha.Corp_S4F3-SCR1B3"

  app "SCR1B3.app"

  zap trash: [
    "~/Library/Application Support/com.itashacorp.scr1b3",
  ]
end
