// Run with node --test tests/gnome-capture.mjs. Exercise the actual extension
// handler with Shell/GIO doubles; compositor rendering still needs GNOME E2E.
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {test} from 'node:test';
import vm from 'node:vm';

const source = readFileSync(new URL('../desktop/gnome/extension.js', import.meta.url), 'utf8')
    .replace(/^import .*;\n/gm, '')
    .replace('export default class Capture', 'class Capture');

test('window capture excludes compositor cursor and closes stream on success or error', async () => {
    for (const fail of [false, true]) {
        let closed = false;
        let captured = false;
        const stream = {close() { closed = true; }};
        const window = {get_stable_sequence: () => 9, activate() {}};
        const context = vm.createContext({
            Extension: class {},
            Gio: {
                File: {new_for_path: () => ({replace: () => stream})},
                FileCreateFlags: {REPLACE_DESTINATION: 1},
            },
            GLib: {timeout_add: (_priority, _ms, callback) => callback()},
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
