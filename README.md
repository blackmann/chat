# Chat

A better vibecoding IDE.

Run `cargo run` for a dev build. To install on your Mac, `./scripts/install-app.sh`.

The app is signed with a code-signing certificate from your keychain so macOS keeps its permission grants across updates. If you have more than one, run `./scripts/install-app.sh --choose-identity` once to pick which to use; the choice is saved to `.signing-identity` (gitignored). `CHAT_SIGN_IDENTITY` overrides it for a single build.
