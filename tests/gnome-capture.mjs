// Run with node --test tests/gnome-capture.mjs. Exercise the actual extension
// handler with Shell/GIO doubles; compositor rendering still needs GNOME E2E.
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {test} from 'node:test';
import vm from 'node:vm';

const source = readFileSync(new URL('../desktop/gnome/extension.js', import.meta.url), 'utf8')
    .replace(/^import .*;\n/gm, '')
    .replace('export default class Capture', 'class Capture');

test('VLC first-run dialog blocks main-window resize, capture and input with identities', async () => {
    // VLC 3.0.23/Xwayland reports the privacy dialog without a transient parent.
    // Do not infer that focus on another window of the same app is safe.
    const appId = 'org.videolan.VLC';
    const makeWindow = (id, title, type) => ({
        get_stable_sequence: () => id, get_title: () => title,
        get_sandboxed_app_id: () => appId, get_pid: () => 392,
        get_transient_for: () => null, get_window_type: () => type,
        is_hidden: () => false, minimized: false, activate() {},
    });
    const main = makeWindow(2, 'VLC media player', 0);
    const dialog = makeWindow(1, 'Privacy and Network Access Policy', 4);
    let time = 0;
    const context = vm.createContext({
        Extension: class {},
        Meta: {WindowType: {MODAL_DIALOG: 4}},
        GLib: {get_monotonic_time: () => time,
            timeout_add: (_priority, ms, callback) => { time += ms * 1000; callback(); }},
        Main: {overview: {hide() {}}, modalCount: 0},
        global: {get_current_time: () => 0, display: {focus_window: dialog}},
    });
    const extension = vm.runInContext(`${source}\nnew Capture()`, context);
    extension._windows = () => [main, dialog];
    for (const action of ['resize', 'capture', 'click', 'key']) {
        await assert.rejects(extension._call({action, window: 2, app_id: appId}), error => {
            assert.match(error.message, /^An application modal dialog blocks the selected window: /);
            const state = JSON.parse(error.message.split(': ').slice(1).join(': '));
            assert.equal(state.selected.title, 'VLC media player');
            assert.equal(state.focused.title, 'Privacy and Network Access Policy');
            assert.equal(state.focused.transient_for, null);
            assert.equal(state.focused.app_id, appId);
            assert.equal(state.selected.hidden, false);
            assert.equal(state.windows.length, 2);
            return true;
        });
    }
    await assert.rejects(extension._call({action: 'key', window: 2, app_id: 'other.app'}),
        /does not belong/);
    // X11 may temporarily focus the main window even though Qt's application
    // modal still blocks input. Focus equality alone must not permit capture.
    context.global.display.focus_window = main;
    await assert.rejects(extension._call({action: 'capture', window: 2, app_id: appId}),
        /application modal/);
    context.global.display.focus_window = dialog;
    // A recipe can explicitly select the dialog, but desktop modals still block it.
    extension._info = window => ({id: window.get_stable_sequence()});
    assert.deepEqual(await extension._call({action: 'focus', window: 1, app_id: appId}), {id: 1});
    context.Main.modalCount = 1;
    await assert.rejects(extension._call({action: 'key', window: 1, app_id: appId}), /desktop modal/);
});

test('an unparented modal permits its focused transient descendants but blocks unrelated windows', async () => {
    const appId = 'org.example.App';
    const makeWindow = (id, parent, type = 4) => ({
        get_stable_sequence: () => id, get_title: () => `Window ${id}`,
        get_sandboxed_app_id: () => appId, get_pid: () => 100,
        get_transient_for: () => parent, get_window_type: () => type,
        is_hidden: () => false, minimized: false, activate() {},
    });
    const preferences = makeWindow(1, null);
    const chooser = makeWindow(2, preferences);
    const confirmation = makeWindow(3, chooser);
    const unrelated = makeWindow(4, null, 0);
    const display = {focus_window: chooser};
    const context = vm.createContext({
        Extension: class {}, Meta: {WindowType: {MODAL_DIALOG: 4}},
        GLib: {get_monotonic_time: () => 0,
            timeout_add: (_priority, _ms, callback) => callback()},
        Main: {overview: {hide() {}}, modalCount: 0},
        global: {get_current_time: () => 0, display},
    });
    const extension = vm.runInContext(`${source}\nnew Capture()`, context);
    let windows = [preferences, chooser, unrelated];
    extension._windows = () => windows;
    extension._info = window => ({id: window.get_stable_sequence()});
    assert.deepEqual(await extension._call({action: 'focus', window: 2, app_id: appId}), {id: 2});
    windows = [...windows, confirmation];
    display.focus_window = confirmation;
    assert.deepEqual(await extension._call({action: 'focus', window: 3, app_id: appId}), {id: 3});
    let pressed = 0;
    extension._key = (_key, down) => { if (down) pressed++; };
    await extension._call({action: 'key', window: 3, app_id: appId, keys: [13]});
    assert.equal(pressed, 1);
    for (const id of [1, 2, 4]) {
        display.focus_window = windows.find(window => window.get_stable_sequence() === id);
        await assert.rejects(extension._call({action: 'key', window: id, app_id: appId, keys: [13]}),
            /application modal/);
    }
    assert.equal(pressed, 1, 'blocked parents and unrelated windows must not receive input');
});

test('a nested modal blocks ancestors through a non-modal dialog', async () => {
    const appId = 'org.example.App';
    const makeWindow = (id, parent, type) => ({
        get_stable_sequence: () => id, get_title: () => `Window ${id}`,
        get_sandboxed_app_id: () => appId, get_pid: () => 100,
        get_transient_for: () => parent, get_window_type: () => type,
        is_hidden: () => false, minimized: false, activate() {},
    });
    const main = makeWindow(1, null, 0);
    const dialog = makeWindow(2, main, 3);
    const modal = makeWindow(3, dialog, 4);
    const display = {focus_window: main};
    const context = vm.createContext({
        Extension: class {}, Meta: {WindowType: {MODAL_DIALOG: 4}},
        GLib: {get_monotonic_time: () => 0},
        Main: {overview: {hide() {}}, modalCount: 0},
        global: {get_current_time: () => 0, display},
    });
    const extension = vm.runInContext(`${source}\nnew Capture()`, context);
    extension._windows = () => [main, dialog, modal];
    extension._info = window => ({id: window.get_stable_sequence()});
    let dispatched = false;
    extension._chord = async () => { dispatched = true; };
    for (const window of [main, dialog]) {
        display.focus_window = window;
        for (const action of ['key', 'capture']) {
            await assert.rejects(extension._call({action, window: window.get_stable_sequence(), app_id: appId}),
                /application modal/);
        }
    }
    assert.equal(dispatched, false);
    display.focus_window = modal;
    assert.deepEqual(await extension._call({action: 'focus', window: 3, app_id: appId}), {id: 3});
});

test('focus timeout and window disappearance cannot dispatch input', async () => {
    for (const disappears of [false, true]) {
        let time = 0;
        const window = {
            get_stable_sequence: () => 2, get_sandboxed_app_id: () => 'org.videolan.VLC',
            get_title: () => 'VLC media player', get_pid: () => 392,
            get_transient_for: () => null, get_window_type: () => 0,
            is_hidden: () => true, minimized: false, activate() {},
        };
        const context = vm.createContext({
            Extension: class {},
            GLib: {get_monotonic_time: () => time,
                timeout_add: (_priority, ms, callback) => { time += ms * 1000; callback(); }},
            Main: {overview: {hide() {}}, modalCount: 0},
            global: {get_current_time: () => 0, display: {focus_window: null}},
        });
        const extension = vm.runInContext(`${source}\nnew Capture()`, context);
        extension._windows = () => disappears && time >= 150_000 ? [] : [window];
        await assert.rejects(extension._call({action: 'key', window: 2}),
            disappears ? /no longer exists/ : /Could not focus selected window.*"focused":null/);
        assert.equal(time, disappears ? 150_000 : 2_000_000);
    }
});

test('Xwayland focus waits for mapping and delayed focus without replaying resize', async () => {
    for (const mapsLate of [false, true]) {
        let time = 0;
        let activations = 0;
        let resizes = 0;
        const display = {focus_window: null};
        const window = {
            get_stable_sequence: () => 2,
            get_sandboxed_app_id: () => 'org.videolan.VLC',
            get_transient_for: () => null,
            is_hidden: () => mapsLate && time < 250_000,
            activate() {
                activations++;
                if (mapsLate && !this.is_hidden()) display.focus_window = this;
            },
            unmaximize() {}, move_resize_frame() { resizes++; },
        };
        const context = vm.createContext({
            Extension: class {}, Meta: {MaximizeFlags: {BOTH: 3}},
            GLib: {get_monotonic_time: () => time,
                timeout_add: (_priority, ms, callback) => {
                    time += ms * 1000;
                    if (!mapsLate && time >= 250_000) display.focus_window = window;
                    callback();
                }},
            Main: {overview: {hide() {}}, modalCount: 0},
            global: {get_current_time: () => 0, display},
        });
        const extension = vm.runInContext(`${source}\nnew Capture()`, context);
        extension._windows = () => [window];
        extension._info = () => ({id: 2});
        assert.deepEqual(await extension._call({action: 'resize', window: 2, width: 900, height: 600}), {id: 2});
        assert.equal(resizes, 1);
        assert.equal(activations, mapsLate ? 2 : 1);
        assert.ok(time < 2_000_000);
    }
});

test('window capture excludes compositor cursor and closes stream on success or error', async () => {
    for (const fail of [false, true]) {
        let closed = false;
        let captured = false;
        const stream = {close() { closed = true; }};
        const window = {get_stable_sequence: () => 9, get_sandboxed_app_id: () => '',
            get_transient_for: () => null, is_hidden: () => false, activate() {}};
        const context = vm.createContext({
            Extension: class {},
            Gio: {
                File: {new_for_path: () => ({replace: () => stream})},
                FileCreateFlags: {REPLACE_DESTINATION: 1},
            },
            GLib: {get_monotonic_time: () => 0, timeout_add: (_priority, _ms, callback) => callback()},
            Main: {overview: {hide() {}}, modalCount: 0},
            global: {get_current_time: () => 0, display: {focus_window: window}},
            Shell: {Screenshot: class {
                screenshot_window(frame, cursor, destination, callback) {
                    assert.equal(frame, true);
                    assert.equal(cursor, false, 'pointer-only desktop must not become window content');
                    assert.equal(destination, stream);
                    captured = true;
                    callback(this, {});
                }
                screenshot_window_finish() {
                    if (fail) throw new Error('capture failed');
                }
            }},
        });
        const extension = vm.runInContext(`${source}\nnew Capture()`, context);
        extension._windows = () => [window];
        extension._info = () => ({id: 9});
        const capture = extension._call({action: 'capture', window: 9, path: '/unused.png'});
        if (fail) await assert.rejects(capture, /capture failed/);
        else assert.deepEqual(await capture, {id: 9});
        assert.equal(captured, true);
        assert.equal(closed, true);
    }
});
