import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Clutter from 'gi://Clutter';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as Config from 'resource:///org/gnome/shell/misc/config.js';

const INTERFACE = `<node><interface name="org.flatpak.Smoke.Desktop1">
<method name="Call"><arg type="s" direction="in"/><arg type="s" direction="out"/></method>
</interface></node>`;

function delay(ms) {
    return new Promise(resolve => GLib.timeout_add(GLib.PRIORITY_DEFAULT, ms, () => {
        resolve();
        return GLib.SOURCE_REMOVE;
    }));
}

export default class Capture extends Extension {
    enable() {
        this._object = Gio.DBusExportedObject.wrapJSObject(INTERFACE, this);
        this._object.export(Gio.DBus.session, '/org/flatpak/Smoke/Desktop1');
        this._healthPath = GLib.getenv('FLATPAK_SMOKE_HELPER_HEALTH_FILE');
        this._name = Gio.bus_own_name_on_connection(Gio.DBus.session,
            'org.flatpak.Smoke.Desktop1', Gio.BusNameOwnerFlags.NONE,
            () => {
                if (this._healthPath)
                    GLib.file_set_contents(this._healthPath, '1');
            }, () => this._removeHealthFile());
        const seat = Clutter.get_default_backend().get_default_seat();
        this._pointer = seat.create_virtual_device(Clutter.InputDeviceType.POINTER_DEVICE);
        this._keyboard = seat.create_virtual_device(Clutter.InputDeviceType.KEYBOARD_DEVICE);
        Main.overview.hide();
    }

    disable() {
        this._removeHealthFile();
        this._object?.unexport();
        if (this._name)
            Gio.bus_unown_name(this._name);
        this._keyboard = null;
        this._pointer = null;
    }

    _removeHealthFile() {
        if (this._healthPath) {
            try { Gio.File.new_for_path(this._healthPath).delete(null); }
            catch (error) {
                if (!error.matches(Gio.IOErrorEnum, Gio.IOErrorEnum.NOT_FOUND))
                    console.error(error);
            }
        }
    }

    async CallAsync([request], invocation) {
        try {
            const result = await this._call(JSON.parse(request));
            invocation.return_value(new GLib.Variant('(s)', [JSON.stringify({ok: true, result})]));
        } catch (error) {
            invocation.return_value(new GLib.Variant('(s)', [JSON.stringify({ok: false, error: error.message})]));
        }
    }

    _windows() {
        return global.get_window_actors().map(actor => actor.meta_window)
            .filter(window => !window.is_override_redirect());
    }

    _info(window) {
        const frame = window.get_frame_rect();
        const buffer = window.get_buffer_rect();
        return {
            id: window.get_stable_sequence(), title: window.get_title() ?? '',
            app_id: window.get_sandboxed_app_id() ?? '', pid: window.get_pid(),
            transient: window.get_transient_for() !== null,
            frame: {x: frame.x, y: frame.y, width: frame.width, height: frame.height},
            buffer: {x: buffer.x, y: buffer.y, width: buffer.width, height: buffer.height},
            scale: window.get_compositor_private().get_resource_scale(),
        };
    }

    _key(value, pressed) {
        this._keyboard.notify_keyval(GLib.get_monotonic_time(), value,
            pressed ? Clutter.KeyState.PRESSED : Clutter.KeyState.RELEASED);
    }

    async _chord(keys) {
        for (const key of keys)
            this._key(key, true);
        await delay(40);
        for (const key of [...keys].reverse())
            this._key(key, false);
    }

    async _call(request) {
        if (request.action === 'ping')
            return {protocol: 1, desktop: 'gnome', version: Config.PACKAGE_VERSION};
        if (request.action === 'windows')
            return this._windows().map(window => this._info(window));
        const window = this._windows().find(candidate => candidate.get_stable_sequence() === request.window);
        if (!window)
            throw new Error('Selected window no longer exists');
        if (request.app_id && window.get_sandboxed_app_id() !== request.app_id)
            throw new Error('Selected window does not belong to the launched app');
        Main.overview.hide();
        window.activate(global.get_current_time());
        await delay(100);
        if (global.display.focus_window !== window)
            throw new Error('Could not focus selected window');
        if (Main.modalCount > 0)
            throw new Error('A desktop modal dialog blocks the selected window');
        if (request.action === 'focus')
            return this._info(window);
        if (request.action === 'resize') {
            window.unmaximize(Meta.MaximizeFlags.BOTH);
            window.move_resize_frame(true, 80, 80, request.width, request.height);
            await delay(200);
            return this._info(window);
        }
        if (request.action === 'capture') {
            const stream = Gio.File.new_for_path(request.path).replace(null, false,
                Gio.FileCreateFlags.REPLACE_DESTINATION, null);
            const shot = new Shell.Screenshot();
            try {
                await new Promise((resolve, reject) => {
                    shot.screenshot_window(true, false, stream, (source, result) => {
                        try { source.screenshot_window_finish(result); resolve(); }
                        catch (error) { reject(error); }
                    });
                });
            } finally {
                stream.close(null);
            }
            return this._info(window);
        }
        if (request.action === 'click') {
            const rect = window.get_buffer_rect();
            this._pointer.notify_absolute_motion(GLib.get_monotonic_time(), rect.x + request.x, rect.y + request.y);
            await delay(50);
            this._pointer.notify_button(GLib.get_monotonic_time(), 1, Clutter.ButtonState.PRESSED);
            await delay(50);
            this._pointer.notify_button(GLib.get_monotonic_time(), 1, Clutter.ButtonState.RELEASED);
            await delay(100);
            this._pointer.notify_absolute_motion(GLib.get_monotonic_time(), 1, 1);
            return {};
        }
        if (request.action === 'key') {
            await this._chord(request.keys);
            return {};
        }
        throw new Error(`Unsupported action ${request.action}`);
    }
}
