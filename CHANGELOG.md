## [1.2.0](https://github.com/estebangarcia/tonn/compare/v1.1.0...v1.2.0) (2026-10-29)

### Features

* add scroll speed setting and use native macOS wheel acceleration ([06167f6](https://github.com/estebangarcia/tonn/commit/06167f6380e41378e039d8ef2d6bbc182df6c6bb))
* freeze scrolled-back panes and show scroll position badge ([bbcf18a](https://github.com/estebangarcia/tonn/commit/bbcf18a7cb1120cc1f5e7f7ab2a241e03033e6d6))
* search pane text with Cmd+F / Ctrl+Shift+F ([5072231](https://github.com/estebangarcia/tonn/commit/50722313e2beb4eb1c3c80b3fb0de73bbcccf2c3))
* show descriptive session titles like the Claude Code VS Code extension ([becc496](https://github.com/estebangarcia/tonn/commit/becc4966442084f63196dc7734c7003d51aa9a09))

### Bug Fixes

* forward mouse and focus events to TUIs and keep inline history in scrollback ([84daa91](https://github.com/estebangarcia/tonn/commit/84daa914590f6e133bfa57dd3d61a800fae0c664))
* keep panes responsive after quitting full-screen apps in a resized pane ([fb3f97a](https://github.com/estebangarcia/tonn/commit/fb3f97ae8e1f1f706abb01d0af4f360cb5ca08f2))
* only bracket pastes when the app enables bracketed paste ([87c3f4a](https://github.com/estebangarcia/tonn/commit/87c3f4aec69226dee07ad78204a6e60425a8055e))
* only target visible panes for focus while a pane is zoomed ([11e4d43](https://github.com/estebangarcia/tonn/commit/11e4d435a9233acd754b316f65e4526df439e025))
* pin semantic-release plugins ([a24c736](https://github.com/estebangarcia/tonn/commit/a24c736ef2cb1ab0a27f6d5a47a3ef7f2599be12))
* release GPU resources before the event loop exits to avoid Wayland segfault ([ebb48c9](https://github.com/estebangarcia/tonn/commit/ebb48c94102532428491a28a03d627f9c88c6529))
* stop session watcher CPU spin and bound per-command output memory ([52abe3e](https://github.com/estebangarcia/tonn/commit/52abe3e857af9769eb3e5906efe9d11cc85fa00c))

## [1.1.0](https://github.com/estebangarcia/tonn/compare/v1.0.4...v1.1.0) (2026-04-14)

### Features

* implement reflow ([15d5e15](https://github.com/estebangarcia/tonn/commit/15d5e15bf16399234ffe6de69092ee2d86ebdac4))
* replace alacritty_terminal with our own emulator ([d019c48](https://github.com/estebangarcia/tonn/commit/d019c48a0a391852b278862c0886eed910a4fba2))

### Bug Fixes

* adding app icon ([3953a3c](https://github.com/estebangarcia/tonn/commit/3953a3c89044745a539070e444bc3917c3e37e4c))
* mark panes as dirty on tab close ([4bf69f8](https://github.com/estebangarcia/tonn/commit/4bf69f8b7481a1a8b529e9d7fbc8840dffcd4a3d))
* release is manual. react on scale factor changes ([15ea360](https://github.com/estebangarcia/tonn/commit/15ea3603936cd285bfdec6fbc8f0f4f5b5a58182))
* zoom-in pane stays when switching tab. resizing window or pane keeps history on the same posiiton ([36d9963](https://github.com/estebangarcia/tonn/commit/36d9963152cc3becbf502d9f6a4aa7372a3fb8bf))

## [1.0.4](https://github.com/estebangarcia/tonn/compare/v1.0.3...v1.0.4) (2026-04-08)

### Bug Fixes

* clippy fix ([629f8d0](https://github.com/estebangarcia/tonn/commit/629f8d0bf2af00bc4431e5e03def3359be76fba4))

## [1.0.3](https://github.com/estebangarcia/tonn/compare/v1.0.2...v1.0.3) (2026-04-08)

### Bug Fixes

* bugs with scrolling, key binding, background gpu redraws ([e08c770](https://github.com/estebangarcia/tonn/commit/e08c770de351e22fe0eb16b079656ace6d64a78c))

## [1.0.2](https://github.com/estebangarcia/tonn/compare/v1.0.1...v1.0.2) (2026-04-07)

### Bug Fixes

* make sure macOS bundles source profiles correctly ([57c31b2](https://github.com/estebangarcia/tonn/commit/57c31b2354e8fe8dd32aa718a28cbba2eb36bb1d))

## [1.0.1](https://github.com/estebangarcia/tonn/compare/v1.0.0...v1.0.1) (2026-04-07)

### Bug Fixes

* release pipeline builds all archs correctly ([bac9bec](https://github.com/estebangarcia/tonn/commit/bac9bec2aa3e219484a2eda8c2fb8db1f858ab06))

## 1.0.0 (2026-04-07)

### Features

* AI Session Browser ([#1](https://github.com/estebangarcia/tonn/issues/1)) ([6f68913](https://github.com/estebangarcia/tonn/commit/6f6891312c8d5752b71d46f30e7a3c736d0bfc0c))
* block UI, tab system, tab switcher, multi-pane support ([065d264](https://github.com/estebangarcia/tonn/commit/065d264a156136dcb85d0a835f2365362d6aecdb))
* compressors for LLM outputs ([ad01917](https://github.com/estebangarcia/tonn/commit/ad019176ea427da9dde63141fc4ef4984d329991))
* initial commit ([77c085a](https://github.com/estebangarcia/tonn/commit/77c085a0765f0740a6ead835a557c3636c6089d6))
* Settings UI ([#2](https://github.com/estebangarcia/tonn/issues/2)) ([220707c](https://github.com/estebangarcia/tonn/commit/220707c07ec41300b74a13730c8982fa7c330f29))
* token-saving pipeline with output classification. built-in MCP server ([7700dd3](https://github.com/estebangarcia/tonn/commit/7700dd345e84481d75d743f1ac9bef4af956d594))
* visual bell and window title update ([867c622](https://github.com/estebangarcia/tonn/commit/867c62280df9d5b8737d8ebf9a98cf1ac7b0fc83))

### Bug Fixes

* various fixes. colour support. powerline support ([c52e5a9](https://github.com/estebangarcia/tonn/commit/c52e5a9bce37899b90274a67025dba18cac28134))
