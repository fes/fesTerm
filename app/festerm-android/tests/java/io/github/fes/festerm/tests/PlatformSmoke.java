package io.github.fes.festerm.tests;

import android.app.*;
import android.content.*;
import android.graphics.Bitmap;
import android.graphics.Color;
import android.media.MediaExtractor;
import android.media.MediaFormat;
import android.media.MediaMetadataRetriever;
import android.net.Uri;
import android.os.*;
import android.provider.DocumentsContract;
import android.system.Os;
import android.system.OsConstants;
import android.text.InputType;
import android.view.*;
import android.view.accessibility.AccessibilityNodeInfo;
import android.view.inputmethod.*;
import io.github.fes.festerm.FestermActivity;
import io.github.fes.festerm.RecordingEncoder;
import java.io.*;
import java.lang.reflect.*;
import java.nio.ByteBuffer;
import java.nio.charset.StandardCharsets;
import java.util.*;
import java.util.concurrent.atomic.AtomicReference;

/** Runs in a separate test APK; no test entry points ship in the application. */
public final class PlatformSmoke extends Instrumentation {
    private boolean recordingValidation;
    private boolean keyboardLifecycleValidation;

    @Override public void onCreate(Bundle args) {
        super.onCreate(args);
        recordingValidation = "true".equals(args.getString("recording-validation"));
        keyboardLifecycleValidation = "true".equals(args.getString("keyboard-lifecycle"));
        if (keyboardLifecycleValidation && recordingValidation) {
            throw new IllegalArgumentException("Select one native validation scope");
        }
        start();
    }

    private static void require(boolean condition, String label) {
        if (!condition) throw new AssertionError(label);
    }

    private void onMain(Runnable action) {
        AtomicReference<Throwable> failure = new AtomicReference<>();
        runOnMainSync(() -> {
            try { action.run(); } catch (Throwable error) { failure.set(error); }
        });
        if (failure.get() != null) throw new AssertionError(failure.get());
    }

    private static void keyboardImeOwnership(FestermActivity activity) {
        int ime = WindowManager.LayoutParams.FLAG_ALT_FOCUSABLE_IM;
        int secure = WindowManager.LayoutParams.FLAG_SECURE;
        activity.keyboardMode(0);
        require((activity.getWindow().getAttributes().flags & ime) != 0,
            "initial/equal Hidden mode excludes system IME target");
        activity.privacy(true, false, true);
        for (int mode : new int[]{2, 1, 4, 3, 0, 3, 2, 4, 1, 0}) {
            activity.keyboardMode(mode);
            int flags = activity.getWindow().getAttributes().flags;
            boolean customOrHidden = mode == 0 || mode == 2 || mode == 4;
            require(((flags & ime) != 0) == customOrHidden,
                "window IME target matches keyboard mode " + mode);
            require((flags & secure) != 0, "keyboard mode retains secure window policy");
            activity.keyboardMode(mode);
            require(activity.getWindow().getAttributes().flags == flags,
                "equal keyboard mode preserves window flags");
        }
        activity.privacy(false, false, false);
    }

    /** EditorInfo/context only; direct terminal callbacks run after privacy fixtures. */
    private static void terminalEditorContract(FestermActivity activity, View input) throws Exception {
        for (int mode : new int[]{1, 2}) {
            activity.keyboardMode(mode);
            EditorInfo info = new EditorInfo();
            InputConnection terminal = input.onCreateInputConnection(info);
            require(info.inputType == InputType.TYPE_NULL, "terminal inputType is exactly TYPE_NULL in mode " + mode);
            require(info.initialSelStart == -1 && info.initialSelEnd == -1,
                "terminal cursor is unknown rather than the start of a local document");
            if (Build.VERSION.SDK_INT >= 31) {
                require(terminal.getSurroundingText(50, 50, 0) == null, "terminal cannot publish a local surrounding-text offset");
            }
            if (Build.VERSION.SDK_INT >= 33) {
                require(terminal.takeSnapshot() == null, "terminal snapshot cannot replace unknown cursor with local zero");
            }
            require((info.imeOptions & EditorInfo.IME_FLAG_NO_PERSONALIZED_LEARNING) != 0,
                "terminal retains no personalized learning");
            require(terminal.getTextBeforeCursor(50, 0).length() == 0, "terminal has no retrievable local document");
        }
        for (int mode : new int[]{3, 4}) {
            for (int kind = 0; kind < 3; kind++) {
                boolean secure = kind == 1;
                boolean multiline = kind == 2;
                activity.fieldDocument(2310 + mode * 3 + kind, "form", 2, 2, secure, multiline);
                activity.keyboardMode(mode);
                EditorInfo info = new EditorInfo();
                InputConnection field = input.onCreateInputConnection(info);
                int expected = secure
                    ? InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_PASSWORD
                    : InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS;
                if (multiline) expected |= InputType.TYPE_TEXT_FLAG_MULTI_LINE;
                require(info.inputType == expected, "form rich/secure inputType unchanged in mode " + mode);
                require(info.initialSelStart == 2 && info.initialSelEnd == 2, "form selection remains retrievable");
                require(field.getTextBeforeCursor(10, 0).length() == 2, "form retains local context");
                if (Build.VERSION.SDK_INT >= 31) {
                    SurroundingText surrounding = field.getSurroundingText(10, 10, 0);
                    require(surrounding != null && surrounding.getSelectionStart() == 2
                        && surrounding.getSelectionEnd() == 2, "form surrounding text preserves actual selection");
                }
                if (Build.VERSION.SDK_INT >= 33) {
                    require(field.takeSnapshot() != null, "rich form snapshot remains available");
                }
            }
        }
        activity.keyboardMode(0);
    }

    /** Direct callbacks only; actual Gboard behavior requires native UI taps. */
    private static void terminalUnicodeCallbacks(FestermActivity activity, View input) throws Exception {
        Field sequence = input.getClass().getDeclaredField("sequence");
        sequence.setAccessible(true);
        for (int mode : new int[]{1, 2}) {
            activity.keyboardMode(mode);
            InputConnection terminal = input.onCreateInputConnection(new EditorInfo());
            long before = sequence.getLong(input);
            require(terminal.commitText("é🙂", 1), "non-rich terminal still accepts direct Unicode commits");
            require(sequence.getLong(input) == before + 1, "Unicode commit emits once");
            require(terminal.getTextBeforeCursor(50, 0).length() == 0, "Unicode terminal commit clears temporary context");
            require(terminal.setComposingText("文", 1), "terminal retains direct composition callbacks");
            require(sequence.getLong(input) == before + 2, "composition preedit emits once");
            require(terminal.commitText("字", 1), "terminal composition commits Unicode");
            require(sequence.getLong(input) == before + 3, "composition commit emits once");
            require(terminal.finishComposingText(), "terminal composition completion remains supported");
            require(sequence.getLong(input) == before + 3, "finished committed composition is not repeated");
            require(terminal.getTextBeforeCursor(50, 0).length() == 0, "terminal composition leaves no local document");
            if (Build.VERSION.SDK_INT >= 31) {
                require(terminal.getSurroundingText(50, 50, 0) == null, "committed terminal text never becomes a local document");
            }
            if (Build.VERSION.SDK_INT >= 33) {
                require(terminal.takeSnapshot() == null, "terminal completion leaves no absolute cursor snapshot");
            }
        }
        activity.keyboardMode(0);
    }

    /** Model discarded candidates and old-wrapper close without weakening editor retirement. */
    private static void editorConnectionLifecycle(FestermActivity activity, View input) throws Exception {
        Field sequence = input.getClass().getDeclaredField("sequence");
        sequence.setAccessible(true);
        activity.keyboardMode(0);
        activity.fieldDocument(0, "", 0, 0, false, false);
        activity.keyboardMode(1);
        InputConnection served = input.onCreateInputConnection(new EditorInfo());
        InputConnection discarded = input.onCreateInputConnection(new EditorInfo());
        require(served != discarded, "redundant candidates remain distinct framework close targets");
        long before = sequence.getLong(input);
        require(served.sendKeyEvent(new KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_E)),
            "discarded redundant candidate must not retire the actual served editor");
        InputConnection duringKey = input.onCreateInputConnection(new EditorInfo());
        require(served.sendKeyEvent(new KeyEvent(KeyEvent.ACTION_UP, KeyEvent.KEYCODE_E)),
            "served release stays owned after redundant creation");
        require(sequence.getLong(input) == before + 1, "served soft key emits exactly once");
        duringKey.closeConnection();
        discarded.closeConnection();
        before = sequence.getLong(input);
        require(served.commitText("e", 1) && sequence.getLong(input) == before + 1,
            "closing a discarded candidate leaves the served connection alive");
        require(!discarded.commitText("retired", 1), "closed discarded candidate refuses callbacks");

        // A closed latest candidate can decline the pre-IME route. Existing
        // native fallback is allowed; this Java fixture must not require a
        // claim or infer end-to-end delivery from the JNI sequence alone.
        activity.keyboardMode(0);
        activity.keyboardMode(1);
        InputConnection hardwareServed = input.onCreateInputConnection(new EditorInfo());
        InputConnection hardwareDiscarded = input.onCreateInputConnection(new EditorInfo());
        hardwareDiscarded.closeConnection();
        require(hardwareServed.getTextBeforeCursor(1, 0) != null,
            "served editor remains valid after redundant candidate closes");
        before = sequence.getLong(input);
        boolean claimedDown = input.dispatchKeyEventPreIme(new KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_F));
        boolean claimedUp = input.dispatchKeyEventPreIme(new KeyEvent(KeyEvent.ACTION_UP, KeyEvent.KEYCODE_F));
        require(claimedDown == claimedUp,
            "Java route must not consume only one half of the hardware pair");
        require(sequence.getLong(input) == before + (claimedDown ? 1 : 0),
            "Java emits once if claimed, or leaves the pair to native fallback");
        require(!hardwareDiscarded.commitText("retired", 1),
            "hardware admission never revives a closed framework candidate");

        activity.keyboardMode(3);
        activity.fieldDocument(2500, "", 0, 0, false, false);
        InputConnection old = input.onCreateInputConnection(new EditorInfo());
        InputConnection adopted = input.onCreateInputConnection(new EditorInfo());
        require(adopted.setComposingText("文", 1), "newly adopted editor establishes composition");
        before = sequence.getLong(input);
        old.closeConnection();
        require(sequence.getLong(input) == before, "old wrapper close must not emit newer composition");
        require(((BaseInputConnection) adopted).getEditable().toString().equals("文"), "old close preserves newer editable composition");
        require(!old.finishComposingText() && !old.commitText("retired", 1),
            "closed old wrapper cannot finish or replace newer composition");
        require(adopted.commitText("字", 1) && sequence.getLong(input) == before + 1,
            "newer composition commits exactly once after old wrapper close");
        require(((BaseInputConnection) adopted).getEditable().toString().equals("字"), "replacement preserves form editing semantics");
        adopted.closeConnection();
        InputConnection replacement = input.onCreateInputConnection(new EditorInfo());
        before = sequence.getLong(input);
        adopted.closeConnection();
        require(!adopted.commitText("retired", 1) && replacement.commitText("n", 1),
            "repeated old close cannot retire the next editor");
        require(sequence.getLong(input) == before + 1, "only the live replacement emits");

        // Each existing explicit context cut must retire every candidate, even
        // if the mode or field later returns to the original values.
        for (int cut = 0; cut < 4; cut++) {
            activity.keyboardMode(0);
            activity.fieldDocument(2600 + cut, "", 0, 0, false, false);
            activity.keyboardMode(3);
            InputConnection first = input.onCreateInputConnection(new EditorInfo());
            InputConnection second = input.onCreateInputConnection(new EditorInfo());
            before = sequence.getLong(input);
            if (cut == 0) { activity.keyboardMode(4); activity.keyboardMode(3); }
            if (cut == 1) activity.fieldDocument(2700, "", 0, 0, false, false);
            if (cut == 2) activity.fieldDocument(2600 + cut, "", 0, 0, true, false);
            if (cut == 3) activity.fieldDocument(2600 + cut, "", 0, 0, false, true);
            require(!first.commitText("retired", 1) && !second.commitText("retired", 1),
                "mode/field/security/multiline cut retires all candidates " + cut);
            require(sequence.getLong(input) == before, "retired epoch emits no JNI input " + cut);
            InputConnection fresh = input.onCreateInputConnection(new EditorInfo());
            require(fresh.commitText("f", 1), "new epoch retains a usable editor " + cut);
        }
        activity.keyboardMode(0);
        require(!replacement.commitText("retired", 1), "hidden lifecycle cut retires prior editor");
        activity.fieldDocument(0, "", 0, 0, false, false);
        activity.keyboardMode(1);
        InputConnection beforeWindowLoss = input.onCreateInputConnection(new EditorInfo());
        // A callback contract check, not evidence of actual OS window focus loss.
        input.onWindowFocusChanged(false);
        input.onWindowFocusChanged(true);
        before = sequence.getLong(input);
        require(!beforeWindowLoss.commitText("retired", 1), "window focus return cannot revive prior epoch");
        require(sequence.getLong(input) == before, "window-retired callback emits nothing");
        InputConnection beforeCover = input.onCreateInputConnection(new EditorInfo());
        activity.privacy(true, true, true);
        Field protect = FestermActivity.class.getDeclaredField("protect");
        Field cover = FestermActivity.class.getDeclaredField("cover");
        protect.setAccessible(true);
        cover.setAccessible(true);
        require(protect.getBoolean(activity) && cover.getBoolean(activity),
            "privacy retirement fixture requires actual covered state");
        activity.privacy(false, false, false);
        before = sequence.getLong(input);
        require(!beforeCover.commitText("retired", 1), "uncover cannot revive prior epoch");
        require(sequence.getLong(input) == before, "privacy-retired callback emits nothing");
        InputConnection afterCover = input.onCreateInputConnection(new EditorInfo());
        require(afterCover.commitText("n", 1), "fresh uncovered editor remains usable");
        activity.keyboardMode(0);
    }

    /** Guards used by deferred IME requests; visibility still needs native UI evidence. */
    private static void systemImeShowGuard(FestermActivity activity, View input) throws Exception {
        Method canShow = input.getClass().getDeclaredMethod("canShowSystemIme", int.class, long.class);
        canShow.setAccessible(true);
        activity.fieldDocument(2300, "", 0, 0, false, false);
        activity.keyboardMode(3);
        require(input.hasFocus() && input.hasWindowFocus(), "system editor owns focused window");
        require((Boolean) canShow.invoke(input, 3, 2300L), "current field can request system IME");
        require(!(Boolean) canShow.invoke(input, 1, 2300L), "different captured mode refuses");
        activity.fieldDocument(2301, "", 0, 0, false, false);
        require(!(Boolean) canShow.invoke(input, 3, 2300L), "retired field cannot show system IME");
        require((Boolean) canShow.invoke(input, 3, 2301L), "replacement field owns its own request");
        activity.privacy(true, true, true);
        require(!(Boolean) canShow.invoke(input, 3, 2301L), "privacy cover cancels system IME intent");
        activity.privacy(false, true, false);
        require(!(Boolean) canShow.invoke(input, 3, 2301L), "covered view cannot request system IME");
        activity.privacy(false, false, false);
        for (String name : new String[]{"ready", "resumed", "destroyed"}) {
            Field lifecycle = FestermActivity.class.getDeclaredField(name);
            lifecycle.setAccessible(true);
            boolean original = lifecycle.getBoolean(activity);
            try {
                lifecycle.setBoolean(activity, name.equals("destroyed"));
                require(!(Boolean) canShow.invoke(input, 3, 2301L),
                    "deferred system IME refuses lifecycle cut " + name);
            } finally { lifecycle.setBoolean(activity, original); }
        }
        int focusability = input.getFocusable();
        boolean touchFocus = input.isFocusableInTouchMode();
        try {
            // clearFocus alone may immediately refocus the first focusable view.
            input.setFocusableInTouchMode(false);
            input.setFocusable(false);
            input.clearFocus();
            require(!input.hasFocus(), "focus-loss fixture must really remove view focus");
            require(!(Boolean) canShow.invoke(input, 3, 2301L), "lost editor focus refuses system IME");
        } finally {
            input.setFocusableInTouchMode(touchFocus);
            input.setFocusable(focusability);
            input.requestFocus();
        }
        for (int mode : new int[]{0, 2, 4}) {
            activity.keyboardMode(mode);
            require(!(Boolean) canShow.invoke(input, 3, 2301L),
                "retired system request refuses hidden/custom mode " + mode);
            require(!(Boolean) canShow.invoke(input, mode, 2301L),
                "hidden/custom owner never requests system IME " + mode);
        }
        activity.keyboardMode(1);
        require((Boolean) canShow.invoke(input, 1, 0L), "native terminal mode may show system IME");
        activity.keyboardMode(0);
    }

    /** Same-mode terminal publications recover real view focus, not fake IME ownership. */
    private static void unchangedTerminalFocus(FestermActivity activity, View input) throws Exception {
        Field owner = input.getClass().getDeclaredField("connection");
        Field sequence = input.getClass().getDeclaredField("sequence");
        Method canRestore = input.getClass().getDeclaredMethod("canRestoreTerminalFocus", int.class);
        owner.setAccessible(true);
        sequence.setAccessible(true);
        canRestore.setAccessible(true);
        ViewGroup parent = (ViewGroup) input.getParent();
        View other = new View(activity);
        other.setFocusableInTouchMode(true);
        parent.addView(other, new ViewGroup.LayoutParams(1, 1));
        try {
            activity.keyboardMode(0);
            activity.keyboardMode(1);
            InputConnection original = input.onCreateInputConnection(new EditorInfo());
            for (int frame = 0; frame < 4; frame++) activity.keyboardMode(1);
            require(owner.get(input) == original, "healthy unchanged terminal preserves input owner");
            require(other.requestFocus() && !input.hasFocus(), "sibling really takes terminal view focus");
            require(input.hasWindowFocus(), "restore fixture retains actual foreground window");
            require((Boolean) canRestore.invoke(input, 1), "current unfocused terminal may restore");
            for (String name : new String[]{"ready", "resumed", "destroyed", "cover"}) {
                Field state = FestermActivity.class.getDeclaredField(name);
                state.setAccessible(true);
                boolean before = state.getBoolean(activity);
                try {
                    state.setBoolean(activity, name.equals("destroyed") || name.equals("cover"));
                    activity.keyboardMode(1);
                    require(!input.hasFocus() && owner.get(input) == null,
                        "unchanged terminal refuses invalid lifecycle/privacy " + name);
                } finally { state.setBoolean(activity, before); }
            }
            activity.fieldDocument(2400, "", 0, 0, false, false);
            activity.keyboardMode(1);
            require(!input.hasFocus(), "terminal cannot restore a form field owner");
            activity.fieldDocument(0, "", 0, 0, true, false);
            activity.keyboardMode(1);
            require(!input.hasFocus(), "terminal cannot restore a secure document");
            activity.fieldDocument(0, "", 0, 0, false, false);
            InputConnection beforeRestore = input.onCreateInputConnection(new EditorInfo());
            long before = sequence.getLong(input);
            activity.keyboardMode(1);
            require(input.hasFocus(), "public unchanged-mode publication restores lost terminal focus");
            require(!original.commitText("stale", 1), "focus return never revives the previously served editor");
            require(!beforeRestore.commitText("stale", 1), "focus restoration retires prior connection");
            require(sequence.getLong(input) == before, "restoration and stale input emit no text");
            InputConnection current = input.onCreateInputConnection(new EditorInfo());
            for (int frame = 0; frame < 4; frame++) activity.keyboardMode(1);
            require(owner.get(input) == current, "repeated healthy frames never restart replacement owner");
            require(current.commitText("g", 1) && sequence.getLong(input) == before + 1,
                "restored current owner emits once");
            for (int mode : new int[]{0, 2, 3, 4}) {
                activity.keyboardMode(mode);
                if (mode == 3 || mode == 4) activity.fieldDocument(2401, "", 0, 0, false, false);
                InputConnection untouched = input.onCreateInputConnection(new EditorInfo());
                require(other.requestFocus() && !input.hasFocus(), "negative-mode fixture loses focus");
                activity.keyboardMode(mode);
                require(!input.hasFocus() && owner.get(input) == (mode == 0 ? untouched : null),
                    "same-mode restoration leaves hidden/custom/form modes unchanged " + mode);
                require(!untouched.commitText("retired", 1), "focus loss retires every mode editor " + mode);
            }
        } finally {
            parent.removeView(other);
            activity.keyboardMode(0);
        }
    }

    /** Shift resolves printable Unicode inside the same owner, never through NDK fallback. */
    private static void shiftedPlainKeys(FestermActivity activity, View input) throws Exception {
        Field sequence = input.getClass().getDeclaredField("sequence");
        sequence.setAccessible(true);
        activity.keyboardMode(0);
        activity.fieldDocument(2402, "", 0, 0, false, false);
        activity.keyboardMode(3);
        InputConnection connection = input.onCreateInputConnection(new EditorInfo());
        long before = sequence.getLong(input);
        String expected = "aA_bB";
        KeyCharacterMap map = KeyCharacterMap.load(KeyCharacterMap.VIRTUAL_KEYBOARD);
        for (int index = 0; index < expected.length(); index++) {
            for (KeyEvent event : map.getEvents(new char[]{expected.charAt(index)})) {
                if (event.getKeyCode() == KeyEvent.KEYCODE_SHIFT_LEFT
                        || event.getKeyCode() == KeyEvent.KEYCODE_SHIFT_RIGHT) {
                    require(!input.dispatchKeyEventPreIme(event), "Shift modifier itself retains native path");
                    continue;
                }
                long previous = sequence.getLong(input);
                require(index % 2 == 0 ? input.dispatchKeyEventPreIme(event) : connection.sendKeyEvent(event),
                    "resolved printable uses current single input owner");
                require(sequence.getLong(input) == previous + (event.getAction() == KeyEvent.ACTION_DOWN ? 1 : 0),
                    "printable emits once on down and never on release");
            }
        }
        require(sequence.getLong(input) == before + expected.length(), "five shifted/plain characters emit exactly once");
        require(expected.contentEquals(connection.getTextBeforeCursor(20, 0)), "mixed ingress preserves exact aA_bB order");
        before = sequence.getLong(input);
        for (int code : new int[]{KeyEvent.KEYCODE_ENTER, KeyEvent.KEYCODE_TAB, KeyEvent.KEYCODE_DEL,
                KeyEvent.KEYCODE_FORWARD_DEL, KeyEvent.KEYCODE_ESCAPE, KeyEvent.KEYCODE_DPAD_UP,
                KeyEvent.KEYCODE_DPAD_DOWN, KeyEvent.KEYCODE_DPAD_LEFT, KeyEvent.KEYCODE_DPAD_RIGHT,
                KeyEvent.KEYCODE_MOVE_HOME, KeyEvent.KEYCODE_MOVE_END}) {
            require(!input.dispatchKeyEventPreIme(new KeyEvent(0, 0, KeyEvent.ACTION_DOWN, code, 0, KeyEvent.META_SHIFT_ON)),
                "Shift special/edit/navigation retains native semantics");
        }
        for (int meta : new int[]{KeyEvent.META_CTRL_ON, KeyEvent.META_ALT_ON, KeyEvent.META_META_ON,
                KeyEvent.META_SYM_ON, KeyEvent.META_FUNCTION_ON}) {
            require(!input.dispatchKeyEventPreIme(new KeyEvent(0, 0, KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_C, 0,
                KeyEvent.META_SHIFT_ON | meta)), "Shift with another modifier remains delegated");
        }
        KeyEvent accent = new KeyEvent(0, 0, KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_APOSTROPHE, 0, KeyEvent.META_SHIFT_ON) {
            @Override public int getUnicodeChar() { return KeyCharacterMap.COMBINING_ACCENT | 0x301; }
        };
        require(!input.dispatchKeyEventPreIme(accent), "shifted dead accent remains native composition");
        require(!input.dispatchKeyEventPreIme(new KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_E)),
            "following native composed character remains delegated");
        require(sequence.getLong(input) == before, "all modified/dead-key negatives emit no plain text");
        activity.keyboardMode(0);
    }

    /** Pre-IME owns plain edits before native post-IME, never a value/frame dedup. */
    private static void preImePlainKeys(FestermActivity activity, View input) throws Exception {
        Field sequence = input.getClass().getDeclaredField("sequence");
        sequence.setAccessible(true);
        activity.fieldDocument(1229, "", 0, 0, false, false);
        activity.keyboardMode(3);
        InputConnection field = input.onCreateInputConnection(new EditorInfo());
        long before = sequence.getLong(input);
        for (KeyEvent event : KeyCharacterMap.load(KeyCharacterMap.VIRTUAL_KEYBOARD).getEvents("echo ".toCharArray())) {
            require(input.dispatchKeyEventPreIme(event), "owned hardware prefix is claimed before IME");
        }
        require(field.commitText("abcdefghijklmnopqrstuvwxyz", 1), "soft chunk uses same ordered queue");
        for (KeyEvent event : KeyCharacterMap.load(KeyCharacterMap.VIRTUAL_KEYBOARD).getEvents("0123456789".toCharArray())) {
            require(input.dispatchKeyEventPreIme(event), "owned hardware suffix is claimed before IME");
        }
        require(sequence.getLong(input) == before + 16, "mixed long text emits exactly fifteen key edits and one commit");
        require("echo abcdefghijklmnopqrstuvwxyz0123456789".contentEquals(field.getTextBeforeCursor(50, 0)),
            "all forty-one characters retain exact mixed ingress order");
        activity.keyboardMode(1);
        InputConnection terminal = input.onCreateInputConnection(new EditorInfo());
        before = sequence.getLong(input);
        for (int code : new int[]{KeyEvent.KEYCODE_ENTER, KeyEvent.KEYCODE_DEL}) {
            require(input.dispatchKeyEventPreIme(new KeyEvent(KeyEvent.ACTION_DOWN, code)), "hardware edit claimed before native delivery");
            require(sequence.getLong(input) == ++before, "hardware edit emits exactly once");
            require(input.dispatchKeyEventPreIme(new KeyEvent(KeyEvent.ACTION_UP, code)), "owned hardware release consumed");
            require(sequence.getLong(input) == before, "hardware release never repeats edit");
            require(terminal.sendKeyEvent(new KeyEvent(KeyEvent.ACTION_DOWN, code)), "soft key uses owned input connection");
            require(sequence.getLong(input) == ++before, "soft edit emits exactly once");
            require(terminal.sendKeyEvent(new KeyEvent(KeyEvent.ACTION_UP, code)), "owned soft release consumed");
            require(sequence.getLong(input) == before, "soft release never repeats edit");
        }
        for (int repeat = 0; repeat < 2; repeat++) {
            require(input.dispatchKeyEventPreIme(new KeyEvent(0, 0, KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_ENTER, repeat)),
                "intentional repeated Return retained");
        }
        require(sequence.getLong(input) == before + 2, "repeated Return is never collapsed by value");
        require(input.dispatchKeyEventPreIme(new KeyEvent(KeyEvent.ACTION_UP, KeyEvent.KEYCODE_ENTER)), "repeated Return release");
        before = sequence.getLong(input);
        for (int meta : new int[]{KeyEvent.META_CTRL_ON, KeyEvent.META_ALT_ON, KeyEvent.META_META_ON,
                KeyEvent.META_SHIFT_ON | KeyEvent.META_CTRL_ON,
                KeyEvent.META_SHIFT_ON | KeyEvent.META_ALT_ON,
                KeyEvent.META_SHIFT_ON | KeyEvent.META_META_ON}) {
            require(!input.dispatchKeyEventPreIme(new KeyEvent(0, 0, KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_C, 0, meta)),
                "modified chord delegates before IME");
        }
        for (int code : new int[]{KeyEvent.KEYCODE_BACK, KeyEvent.KEYCODE_VOLUME_UP, KeyEvent.KEYCODE_POWER}) {
            require(!input.dispatchKeyEventPreIme(new KeyEvent(KeyEvent.ACTION_DOWN, code)), "system key delegates before IME");
        }
        KeyEvent dead = new KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_APOSTROPHE) {
            @Override public int getUnicodeChar() { return KeyCharacterMap.COMBINING_ACCENT | 0x301; }
        };
        Field nativeComposition = input.getClass().getDeclaredField("nativeComposition");
        Field connectionComposition = input.getClass().getDeclaredField("connectionComposition");
        nativeComposition.setAccessible(true);
        connectionComposition.setAccessible(true);
        KeyEvent following = new KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_E);
        for (boolean reachesActivity : new boolean[]{false, true}) {
            require(!input.dispatchKeyEventPreIme(dead), "dead accent delegates before IME");
            require(nativeComposition.getBoolean(input), "pre-IME owns native accent even when Activity never sees it");
            if (reachesActivity) activity.dispatchKeyEvent(dead);
            require(nativeComposition.getBoolean(input), "optional Activity dispatch cannot advance hardware composition");
            require(!input.dispatchKeyEventPreIme(following), "following character stays on native composition route");
            require(!nativeComposition.getBoolean(input), "pre-IME retires native sequence once");
            if (reachesActivity) activity.dispatchKeyEvent(following);
            require(!nativeComposition.getBoolean(input), "Activity cannot reclaim declined following character");
            require(sequence.getLong(input) == before, "declined native sequence never emits plain JNI text");
        }
        // Model the soft InputConnection's already-posted fence. Activity only
        // retires that soft fence, leaving hardware composition and JNI alone.
        connectionComposition.setBoolean(input, true);
        activity.dispatchKeyEvent(dead);
        require(connectionComposition.getBoolean(input), "posted soft accent retains its ingress fence");
        require(!nativeComposition.getBoolean(input), "soft observer does not acquire hardware composition");
        activity.dispatchKeyEvent(following);
        require(!connectionComposition.getBoolean(input), "posted soft following character retires only soft fence");
        require(!nativeComposition.getBoolean(input), "soft observer never advances hardware composition");
        activity.dispatchKeyEvent(new KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_X));
        require(sequence.getLong(input) == before, "Activity never routes ordinary fallback keys into JNI");
        for (String name : new String[]{"ready", "resumed", "destroyed"}) {
            Field lifecycle = FestermActivity.class.getDeclaredField(name);
            lifecycle.setAccessible(true);
            boolean original = lifecycle.getBoolean(activity);
            try {
                lifecycle.setBoolean(activity, name.equals("destroyed"));
                require(!input.dispatchKeyEventPreIme(new KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_ENTER)),
                    "pre-IME refuses lifecycle cut " + name);
            } finally { lifecycle.setBoolean(activity, original); }
        }
        int focusability = input.getFocusable();
        boolean touchFocus = input.isFocusableInTouchMode();
        try {
            // clearFocus alone may immediately refocus the first focusable view.
            input.setFocusableInTouchMode(false);
            input.setFocusable(false);
            input.clearFocus();
            require(!input.hasFocus(), "focus-loss fixture must really remove view focus");
            require(!input.dispatchKeyEventPreIme(new KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_ENTER)), "lost focus delegates");
        } finally {
            input.setFocusableInTouchMode(touchFocus);
            input.setFocusable(focusability);
            input.requestFocus();
        }
        activity.privacy(true, true, false);
        require(!input.dispatchKeyEventPreIme(new KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_ENTER)), "covered view cannot claim pre-IME edit");
        activity.privacy(false, false, false);
        activity.keyboardMode(0);
        require(!input.dispatchKeyEventPreIme(new KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_ENTER)), "hidden mode delegates without JNI edit");
        activity.keyboardMode(1);
        Field currentConnection = input.getClass().getDeclaredField("connection");
        currentConnection.setAccessible(true);
        currentConnection.set(input, null);
        require(!input.dispatchKeyEventPreIme(new KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_ENTER)), "missing connection retains native fallback");
        require(!terminal.sendKeyEvent(new KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_ENTER)), "retired connection remains retired");
        require(sequence.getLong(input) == before, "ownership cuts emit nothing");
        activity.keyboardMode(0);
    }

    /** Real Java/IME/JNI path; sequence counts detect duplicate or missing emissions. */
    private static void orderedPlainKeys(FestermActivity activity, View input) throws Exception {
        Field sequence = input.getClass().getDeclaredField("sequence");
        sequence.setAccessible(true);
        Field protectedState = FestermActivity.class.getDeclaredField("protect");
        Field coveredState = FestermActivity.class.getDeclaredField("cover");
        protectedState.setAccessible(true);
        coveredState.setAccessible(true);
        activity.fieldDocument(1233, "", 0, 0, false, false);
        activity.keyboardMode(3);
        InputConnection connection = input.onCreateInputConnection(new EditorInfo());
        require(input.hasFocus() && input.hasWindowFocus(), "owned native input focus");
        long before = sequence.getLong(input);
        require(input.dispatchKeyEventPreIme(new KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_C)),
            "hardware c uses native input queue");
        require(connection.sendKeyEvent(new KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_L)),
            "IME key l shares native input queue");
        require(connection.commitText("e", 1), "IME commit e shares native input queue");
        require(input.dispatchKeyEventPreIme(new KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_A)),
            "hardware a uses native input queue");
        require(input.dispatchKeyEventPreIme(new KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_R)),
            "hardware r uses native input queue");
        require(sequence.getLong(input) == before + 5, "five ordered edits, exactly once each");
        require("clear".contentEquals(connection.getTextBeforeCursor(20, 0)), "mixed ingress retains clear order");
        for (int code : new int[]{KeyEvent.KEYCODE_C, KeyEvent.KEYCODE_A, KeyEvent.KEYCODE_R}) {
            require(input.dispatchKeyEventPreIme(new KeyEvent(KeyEvent.ACTION_UP, code)), "consumed hardware key up");
        }
        require(connection.sendKeyEvent(new KeyEvent(KeyEvent.ACTION_UP, KeyEvent.KEYCODE_L)), "consumed IME key up");
        require(sequence.getLong(input) == before + 5, "key releases never emit duplicate text");
        require(!input.dispatchKeyEventPreIme(new KeyEvent(KeyEvent.ACTION_UP, KeyEvent.KEYCODE_C)),
            "unowned release is delegated");
        for (int meta : new int[]{KeyEvent.META_CTRL_ON, KeyEvent.META_ALT_ON, KeyEvent.META_META_ON}) {
            KeyEvent chord = new KeyEvent(0, 0, KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_C, 0, meta);
            require(!input.dispatchKeyEventPreIme(chord), "modified chord retains NativeActivity path");
        }
        for (int code : new int[]{KeyEvent.KEYCODE_BACK, KeyEvent.KEYCODE_VOLUME_UP, KeyEvent.KEYCODE_POWER}) {
            require(!input.dispatchKeyEventPreIme(new KeyEvent(KeyEvent.ACTION_DOWN, code)),
                "system key retains NativeActivity path");
        }
        KeyEvent dead = new KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_APOSTROPHE) {
            @Override public int getUnicodeChar() { return KeyCharacterMap.COMBINING_ACCENT | 0x301; }
        };
        require(!input.dispatchKeyEventPreIme(dead), "dead accent retains NativeActivity composition path");
        require(!input.dispatchKeyEventPreIme(new KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_E)),
            "accent following character stays on same native composition path");
        require(sequence.getLong(input) == before + 5, "delegation emits no unmodified text");
        require(input.dispatchKeyEventPreIme(new KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_DEL)), "hardware delete");
        require("clea".contentEquals(connection.getTextBeforeCursor(20, 0)), "hardware delete updates field context");
        require(input.dispatchKeyEventPreIme(new KeyEvent(KeyEvent.ACTION_UP, KeyEvent.KEYCODE_DEL)), "delete key up");
        activity.fieldDocument(1236, "••", 2, 2, true, false);
        InputConnection secure = input.onCreateInputConnection(new EditorInfo());
        long secureBefore = sequence.getLong(input);
        require(!connection.sendKeyEvent(new KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_X)), "retired field key refused");
        require(sequence.getLong(input) == secureBefore, "retired connection emits nothing");
        require(!input.dispatchKeyEventPreIme(new KeyEvent(KeyEvent.ACTION_UP, KeyEvent.KEYCODE_R)),
            "field transition clears consumed releases");
        require(input.dispatchKeyEventPreIme(new KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_X)), "secure hardware text");
        require("•••".contentEquals(secure.getTextBeforeCursor(20, 0)), "secure hardware context stays masked");
        require(input.dispatchKeyEventPreIme(new KeyEvent(KeyEvent.ACTION_UP, KeyEvent.KEYCODE_X)), "secure release");
        activity.keyboardMode(2);
        InputConnection terminal = input.onCreateInputConnection(new EditorInfo());
        long terminalBefore = sequence.getLong(input);
        for (int code : new int[]{KeyEvent.KEYCODE_DPAD_LEFT, KeyEvent.KEYCODE_ENTER}) {
            require(input.dispatchKeyEventPreIme(new KeyEvent(KeyEvent.ACTION_DOWN, code)), "terminal edit/navigation key");
            require(input.dispatchKeyEventPreIme(new KeyEvent(KeyEvent.ACTION_UP, code)), "terminal release");
        }
        require(sequence.getLong(input) == terminalBefore + 2, "terminal keys emit once");
        activity.privacy(true, true, false);
        require(protectedState.getBoolean(activity) && coveredState.getBoolean(activity),
            "fixture precondition lost: covered input owner before key");
        long coveredBefore = sequence.getLong(input);
        boolean coveredAccepted = terminal.sendKeyEvent(new KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_X));
        require(protectedState.getBoolean(activity) && coveredState.getBoolean(activity),
            "fixture precondition lost: covered input owner during key");
        require(!coveredAccepted, "covered input connection refuses key instead of delegating");
        require(sequence.getLong(input) == coveredBefore, "covered input emits nothing");
        activity.privacy(false, false, false);
        activity.keyboardMode(0);
        require(!terminal.sendKeyEvent(new KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_X)), "hidden connection refuses key");
        require(!input.dispatchKeyEventPreIme(new KeyEvent(KeyEvent.ACTION_UP, KeyEvent.KEYCODE_X)),
            "hidden mode clears consumed releases");
        activity.keyboardMode(2);
        require(!terminal.sendKeyEvent(new KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_X)),
            "mode roundtrip cannot reactivate retired connection");
        activity.keyboardMode(0);
    }

    /** Synthetic Java/JNI ownership transition, not uninterrupted Rust form ownership. */
    private void postedAccentOwnershipTransition(FestermActivity activity, View input) throws Exception {
        Field sequence = input.getClass().getDeclaredField("sequence");
        Field ingress = input.getClass().getDeclaredField("connectionComposition");
        Field nativeState = input.getClass().getDeclaredField("nativeComposition");
        sequence.setAccessible(true); ingress.setAccessible(true); nativeState.setAccessible(true);
        AtomicReference<InputConnection> original = new AtomicReference<>();
        onMain(() -> {
            try {
                activity.keyboardMode(0);
                activity.fieldDocument(1237, "", 0, 0, false, false);
                activity.keyboardMode(3);
                InputConnection connection = input.onCreateInputConnection(new EditorInfo());
                original.set(connection);
                long before = sequence.getLong(input);
                KeyEvent dead = new KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_APOSTROPHE) {
                    @Override public int getUnicodeChar() { return KeyCharacterMap.COMBINING_ACCENT | 0x301; }
                };
                // BaseInputConnection posts these events but returns false.
                connection.sendKeyEvent(dead);
                connection.sendKeyEvent(new KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_E));
                require(sequence.getLong(input) == before,
                    "same connection emits no JNI plain text before posted work runs");
                require(ingress.getBoolean(input), "ingress fence survives same UI turn");
            } catch (Exception error) { throw new AssertionError(error); }
        });
        // Drain real posted Activity work. Rust/IMM may legitimately retire this
        // synthetic owner; no claim that field1237 remains a current GUI form.
        waitForIdleSync();
        onMain(() -> {
            try {
                activity.keyboardMode(0);
                activity.fieldDocument(1238, "", 0, 0, false, false);
                activity.keyboardMode(3);
                InputConnection fresh = input.onCreateInputConnection(new EditorInfo());
                require(fresh != null && fresh != original.get(), "deliberately fresh fixture owner");
                require(!ingress.getBoolean(input) && !nativeState.getBoolean(input),
                    "ownership transition clears both finite composition fences");
                long before = sequence.getLong(input);
                require(!original.get().sendKeyEvent(new KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_G)),
                    "original synthetic owner cannot reactivate after ownership transition");
                require(sequence.getLong(input) == before, "retired synthetic owner emits no JNI input");
                require(fresh.sendKeyEvent(new KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_G)),
                    "fresh fixture owner accepts plain key");
                require(sequence.getLong(input) == before + 1, "fresh plain key emits exactly once");
                require(fresh.sendKeyEvent(new KeyEvent(KeyEvent.ACTION_UP, KeyEvent.KEYCODE_G)),
                    "fresh plain key release consumed");
                require(sequence.getLong(input) == before + 1, "fresh release emits nothing");
                activity.keyboardMode(0);
            } catch (Exception error) { throw new AssertionError(error); }
        });
    }

    private static void requireColor(Bitmap frame, int x, int y, int red, int green, int blue, String label) {
        int color = frame.getPixel(x, y);
        require(Math.abs(Color.red(color) - red) < 70
            && Math.abs(Color.green(color) - green) < 70
            && Math.abs(Color.blue(color) - blue) < 70, label);
    }

    private static List<Long> videoTimes(File file) throws IOException {
        MediaExtractor extractor = new MediaExtractor();
        try {
            extractor.setDataSource(file.getPath());
            require(extractor.getTrackCount() == 1, "one synchronized video track");
            require("video/avc".equals(extractor.getTrackFormat(0).getString(MediaFormat.KEY_MIME)),
                "synchronized H.264 track");
            extractor.selectTrack(0);
            List<Long> times = new ArrayList<>();
            do {
                long time = extractor.getSampleTime();
                if (time < 0) break;
                times.add(time);
            } while (extractor.advance());
            return times;
        } finally {
            extractor.release();
        }
    }

    private static List<Long> videoTimes(Context context, Uri uri) throws IOException {
        MediaExtractor extractor = new MediaExtractor();
        try {
            extractor.setDataSource(context, uri, null);
            require(extractor.getTrackCount() == 1, "one provider MP4 track");
            require("video/avc".equals(extractor.getTrackFormat(0).getString(MediaFormat.KEY_MIME)),
                "provider MP4 is H.264");
            extractor.selectTrack(0);
            List<Long> times = new ArrayList<>();
            do {
                long time = extractor.getSampleTime();
                if (time < 0) break;
                times.add(time);
            } while (extractor.advance());
            return times;
        } finally {
            extractor.release();
        }
    }

    private static boolean tap(AccessibilityNodeInfo node, String label) {
        if (node == null) return false;
        CharSequence text = node.getText(), description = node.getContentDescription();
        if ((text != null && label.equalsIgnoreCase(text.toString()))
            || (description != null && label.equalsIgnoreCase(description.toString()))) {
            AccessibilityNodeInfo candidate = node;
            while (candidate != null) {
                if (candidate.isEnabled() && candidate.isClickable()
                    && candidate.performAction(AccessibilityNodeInfo.ACTION_CLICK)) return true;
                candidate = candidate.getParent();
            }
        }
        for (int index = 0; index < node.getChildCount(); index++) {
            if (tap(node.getChild(index), label)) return true;
        }
        return false;
    }

    private Uri recordingTree(FestermActivity activity) throws Exception {
        String target = "primary:Documents/fes-recording-00dd";
        for (android.content.UriPermission permission : activity.getContentResolver().getPersistedUriPermissions()) {
            if (permission.isReadPermission() && permission.isWritePermission()
                && DocumentsContract.isTreeUri(permission.getUri())
                && target.equals(DocumentsContract.getTreeDocumentId(permission.getUri()))) return permission.getUri();
        }
        Field request = FestermActivity.class.getDeclaredField("PICK_RECORDING_FOLDER");
        request.setAccessible(true);
        Intent pick = new Intent(Intent.ACTION_OPEN_DOCUMENT_TREE)
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_GRANT_WRITE_URI_PERMISSION
                | Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION | Intent.FLAG_GRANT_PREFIX_URI_PERMISSION)
            .putExtra(DocumentsContract.EXTRA_INITIAL_URI,
                DocumentsContract.buildDocumentUri("com.android.externalstorage.documents", target));
        int code = request.getInt(null);
        onMain(() -> activity.startActivityForResult(pick, code));
        UiAutomation automation = getUiAutomation();
        long deadline = SystemClock.uptimeMillis() + 30000;
        while (SystemClock.uptimeMillis() < deadline) {
            for (android.content.UriPermission permission : activity.getContentResolver().getPersistedUriPermissions()) {
                if (permission.isReadPermission() && permission.isWritePermission()
                    && DocumentsContract.isTreeUri(permission.getUri())
                    && target.equals(DocumentsContract.getTreeDocumentId(permission.getUri()))) return permission.getUri();
            }
            AccessibilityNodeInfo root = automation.getRootInActiveWindow();
            if (!tap(root, "Use this folder")) tap(root, "Allow");
            Thread.sleep(300);
        }
        throw new AssertionError("recording folder picker did not grant the test directory");
    }

    private void providerRecording(FestermActivity activity) throws Exception {
        Uri tree = recordingTree(activity);
        ContentResolver resolver = activity.getContentResolver();
        Uri root = DocumentsContract.buildDocumentUriUsingTree(tree, DocumentsContract.getTreeDocumentId(tree));
        String base = "recording-native-" + UUID.randomUUID().toString();
        Uri existing = DocumentsContract.createDocument(resolver, root,
            DocumentsContract.Document.MIME_TYPE_DIR, base);
        require(existing != null, "create preexisting export fixture");
        Uri keep = DocumentsContract.createDocument(resolver, existing, "text/plain", "keep.txt");
        require(keep != null, "create preexisting export marker");
        try (OutputStream output = resolver.openOutputStream(keep, "w")) {
            require(output != null, "open preexisting export marker");
            output.write("preserve".getBytes(StandardCharsets.UTF_8));
        }
        File parent = new File(activity.recordingDirectory());
        File local = new File(parent, "provider-test-" + UUID.randomUUID());
        require(local.mkdir(), "create provider recording test directory");
        Uri published = null;
        Uri appearance = null;
        List<Uri> documents = new ArrayList<>();
        List<Uri> videos = new ArrayList<>();
        try {
            Class<?> hooks = Class.forName("io.github.fes.festerm.RecordingNativeTest",
                true, activity.getClassLoader());
            String result = (String) hooks.getMethod("publish", String.class, String.class, String.class)
                .invoke(null, local.getPath(), tree.toString(), base);
            require(result != null, "Rust-to-Android provider recording publication");
            published = Uri.parse(result);
            require(!DocumentsContract.getDocumentId(existing).equals(DocumentsContract.getDocumentId(published)),
                "provider publication did not reuse an existing directory");
            Uri children = DocumentsContract.buildChildDocumentsUriUsingTree(
                tree, DocumentsContract.getDocumentId(published));
            try (android.database.Cursor cursor = resolver.query(children,
                    new String[]{DocumentsContract.Document.COLUMN_DOCUMENT_ID,
                        DocumentsContract.Document.COLUMN_DISPLAY_NAME,
                        DocumentsContract.Document.COLUMN_MIME_TYPE}, null, null, null)) {
                require(cursor != null, "list published provider files");
                while (cursor.moveToNext()) {
                    Uri document = DocumentsContract.buildDocumentUriUsingTree(tree, cursor.getString(0));
                    documents.add(document);
                    String name = cursor.getString(1), mime = cursor.getString(2);
                    if ("recording-appearance.json".equals(name)) {
                        require(appearance == null && "application/json".equals(mime),
                            "one correctly typed appearance document");
                        appearance = document;
                    } else {
                        require(name.startsWith("session-") && name.endsWith(".mp4")
                            && "video/mp4".equals(mime), "unexpected provider document: " + name);
                        videos.add(document);
                    }
                }
            }
            require(videos.size() == 2, "two provider MP4s");
            require(appearance != null, "provider appearance document");
            try (InputStream input = resolver.openInputStream(appearance)) {
                require(input != null, "open provider appearance document");
                ByteArrayOutputStream contents = new ByteArrayOutputStream();
                byte[] buffer = new byte[1024];
                int count;
                while ((count = input.read(buffer)) != -1) contents.write(buffer, 0, count);
                org.json.JSONObject metadata = new org.json.JSONObject(
                    new String(contents.toByteArray(), StandardCharsets.UTF_8));
                require("jet_brains_mono".equals(metadata.getString("font"))
                    && metadata.getInt("width") == 256 && metadata.getInt("height") == 128
                    && !metadata.getBoolean("recovered_prefix"), "provider appearance identity");
            }
            List<Long> first = videoTimes(activity, videos.get(0));
            require(first.size() >= 4 && first.equals(videoTimes(activity, videos.get(1))),
                "provider MP4s retain the synchronized frame schedule");
            try (InputStream input = resolver.openInputStream(keep)) {
                require(input != null, "open existing provider marker");
                ByteArrayOutputStream contents = new ByteArrayOutputStream();
                byte[] buffer = new byte[64];
                int count;
                while ((count = input.read(buffer)) != -1) contents.write(buffer, 0, count);
                require(Arrays.equals(contents.toByteArray(), "preserve".getBytes(StandardCharsets.UTF_8)),
                    "existing provider export remains untouched");
            }
        } finally {
            for (Uri document : documents) require(DocumentsContract.deleteDocument(resolver, document),
                "remove provider test document");
            if (published != null) require(DocumentsContract.deleteDocument(resolver, published),
                "remove provider test export");
            require(DocumentsContract.deleteDocument(resolver, keep), "remove preexisting marker");
            require(DocumentsContract.deleteDocument(resolver, existing), "remove preexisting fixture");
            File staging = new File(local, "staging");
            File[] files = staging.listFiles();
            if (files != null) for (File file : files) {
                require(file.getName().startsWith("stream-") && file.delete(), "remove staged test MP4");
            }
            if (staging.exists()) require(staging.delete(), "remove provider staging");
            File sources = new File(local, "sources");
            File[] remaining = sources.listFiles();
            if (remaining != null) for (File file : remaining) {
                require(file.getName().startsWith("recording-") && file.delete(), "remove retained test source");
            }
            if (sources.exists()) require(sources.delete(), "remove provider source directory");
            File[] journals = local.listFiles();
            if (journals != null) for (File journal : journals) {
                if (journal.getName().startsWith("provider-") && journal.getName().endsWith(".json"))
                    require(journal.delete(), "remove provider test journal");
            }
            require(local.delete(), "remove provider test directory");
        }
    }

    private static void nativeRecording(FestermActivity activity, File directory) throws Exception {
        Class<?> hooks = Class.forName("io.github.fes.festerm.RecordingNativeTest",
            true, activity.getClassLoader());
        int status = (Integer) hooks.getMethod("encode", String.class).invoke(null, directory.getPath());
        File staging = new File(directory, "staging");
        File sources = new File(directory, "sources");
        File first = new File(staging, "stream-01.mp4"), second = new File(staging, "stream-02.mp4");
        try {
            require(status == 0, "Rust-to-Java synchronized recording export");
            List<Long> firstTimes = videoTimes(first), secondTimes = videoTimes(second);
            require(firstTimes.size() >= 4 && firstTimes.equals(secondTimes), "shared frame schedule");
            MediaMetadataRetriever red = new MediaMetadataRetriever(), green = new MediaMetadataRetriever();
            try {
                red.setDataSource(first.getPath());
                green.setDataSource(second.getPath());
                Bitmap redFrame = red.getFrameAtTime(100_000, MediaMetadataRetriever.OPTION_CLOSEST);
                Bitmap greenFrame = green.getFrameAtTime(100_000, MediaMetadataRetriever.OPTION_CLOSEST);
                require(redFrame != null && greenFrame != null, "decoded synchronized frames");
                int redPixels = 0, greenPixels = 0;
                for (int y = 0; y < redFrame.getHeight(); y++) for (int x = 0; x < redFrame.getWidth(); x++) {
                    int r = redFrame.getPixel(x, y), g = greenFrame.getPixel(x, y);
                    if (Color.red(r) > 90 && Color.red(r) > Color.green(r) * 3 / 2) redPixels++;
                    if (Color.green(g) > 90 && Color.green(g) > Color.red(g) * 3 / 2) greenPixels++;
                }
                require(redPixels > 10 && greenPixels > 10, "separate rendered terminal streams");
                redFrame.recycle();
                greenFrame.recycle();
            } finally {
                red.release();
                green.release();
            }
            File[] files = sources.listFiles();
            require(files != null && files.length == 1 && files[0].getName().endsWith(".fstrec"),
                "source retained after staging");
        } finally {
            if (first.exists()) require(first.delete(), "remove first Rust recording video");
            if (second.exists()) require(second.delete(), "remove second Rust recording video");
            if (staging.exists()) require(staging.delete(), "remove Rust recording staging");
            File[] files = sources.listFiles();
            if (files != null) for (File file : files) {
                require(file.getName().startsWith("recording-") && file.delete(), "remove Rust recording source");
            }
            if (sources.exists()) require(sources.delete(), "remove Rust recording source directory");
        }
    }

    private static void recordingEncoder(FestermActivity activity, boolean nativeValidation) throws Exception {
        File parent = new File(activity.recordingDirectory());
        require(parent.getCanonicalPath().startsWith(activity.getNoBackupFilesDir().getCanonicalPath() + "/"),
            "recording storage excludes Auto Backup");
        require((Os.stat(parent.getPath()).st_mode & 0777) == 0700, "private recording directory");
        File directory = new File(parent, "native-test-" + UUID.randomUUID());
        require(directory.mkdir(), "create recording test directory");
        File video = new File(directory, "video.mp4");
        File padded = new File(directory, "padded.mp4");
        File link = new File(directory, "link.mp4");
        try {
            try (RecordingEncoder encoder = activity.newRecordingEncoder(video.getPath(), 64, 64)) {
                ByteBuffer frame = ByteBuffer.allocateDirect(64 * 64 * 4);
                for (int index = 0; index < 12; index++) {
                    frame.clear();
                    for (int y = 0; y < 64; y++) for (int x = 0; x < 64; x++) {
                        boolean first = index < 6;
                        int red = first ? (x < 32 && y < 32 ? 255 : 0) : (x >= 32 ? 255 : 0);
                        int green = first ? (x >= 32 && y < 32 ? 255 : 0) : (y < 32 ? 255 : 0);
                        int blue = first ? (y >= 32 ? 255 : 0) : (x < 32 && y >= 32 ? 255 : 0);
                        frame.put((byte) red).put((byte) green).put((byte) blue).put((byte) 255);
                    }
                    frame.flip();
                    encoder.append(frame);
                }
                encoder.finish();
            }
            require((Os.stat(video.getPath()).st_mode & 0777) == 0600, "private video file");
            try (RecordingEncoder ignored = activity.newRecordingEncoder(video.getPath(), 64, 64)) {
                throw new AssertionError("existing video overwritten");
            } catch (IOException expected) {
                require(video.isFile(), "existing video preserved");
            }
            Os.symlink(video.getPath(), link.getPath());
            try (RecordingEncoder ignored = activity.newRecordingEncoder(link.getPath(), 64, 64)) {
                throw new AssertionError("symlink video accepted");
            } catch (IOException expected) {
                require(video.isFile(), "symlink target preserved");
            }

            MediaExtractor extractor = new MediaExtractor();
            try {
                extractor.setDataSource(video.getPath());
                require(extractor.getTrackCount() == 1, "one silent video track");
                MediaFormat format = extractor.getTrackFormat(0);
                require("video/avc".equals(format.getString(MediaFormat.KEY_MIME)), "H.264 track");
                require(format.getInteger(MediaFormat.KEY_WIDTH) == 64
                    && format.getInteger(MediaFormat.KEY_HEIGHT) == 64, "video dimensions");
                extractor.selectTrack(0);
                int count = 0;
                long previous = -1;
                do {
                    long time = extractor.getSampleTime();
                    if (time < 0) break;
                    require(time > previous, "monotonic frame times");
                    require(Math.abs(time - count * 1_000_000L / 30) <= 1, "30 fps timestamps");
                    previous = time;
                    count++;
                } while (extractor.advance());
                require(count == 12, "all frames encoded");
            } finally {
                extractor.release();
            }

            MediaMetadataRetriever retriever = new MediaMetadataRetriever();
            try {
                retriever.setDataSource(video.getPath());
                Bitmap first = retriever.getFrameAtTime(0, MediaMetadataRetriever.OPTION_CLOSEST);
                Bitmap later = retriever.getFrameAtTime(8 * 1_000_000L / 30,
                    MediaMetadataRetriever.OPTION_CLOSEST);
                require(first != null && later != null, "decoded video frames");
                requireColor(first, 16, 16, 255, 0, 0, "top-left red and orientation");
                requireColor(first, 48, 16, 0, 255, 0, "top-right green");
                requireColor(first, 16, 48, 0, 0, 255, "bottom-left blue");
                requireColor(later, 48, 16, 255, 255, 0, "later frame yellow");
                requireColor(later, 16, 48, 0, 0, 255, "later frame blue");
                first.recycle();
                later.recycle();
            } finally {
                retriever.release();
            }

            try (RecordingEncoder encoder = activity.newRecordingEncoder(padded.getPath(), 66, 66)) {
                ByteBuffer frame = ByteBuffer.allocateDirect(66 * 66 * 4);
                for (int index = 0; index < 3; index++) {
                    frame.clear();
                    for (int pixel = 0; pixel < 66 * 66; pixel++) {
                        frame.put((byte) 0).put((byte) 255).put((byte) 0).put((byte) 255);
                    }
                    frame.flip();
                    encoder.append(frame);
                }
                encoder.finish();
            }
            MediaMetadataRetriever paddedRetriever = new MediaMetadataRetriever();
            try {
                paddedRetriever.setDataSource(padded.getPath());
                Bitmap frame = paddedRetriever.getFrameAtTime(0, MediaMetadataRetriever.OPTION_CLOSEST);
                require(frame != null && frame.getWidth() == 66 && frame.getHeight() == 66,
                    "unaligned frame dimensions");
                requireColor(frame, 60, 60, 0, 255, 0, "unaligned frame color and stride");
                frame.recycle();
            } finally {
                paddedRetriever.release();
            }
            if (nativeValidation) nativeRecording(activity, directory);
        } finally {
            if (link.exists()) require(link.delete(), "remove recording test link");
            if (video.exists()) require(video.delete(), "remove recording test video");
            if (padded.exists()) require(padded.delete(), "remove padded recording test video");
            require(directory.delete(), "remove recording test directory");
        }
    }

    private void keyboardLifecycle(FestermActivity activity) throws Exception {
        Field inputField = FestermActivity.class.getDeclaredField("input");
        inputField.setAccessible(true);
        View input = (View) inputField.get(activity);
        onMain(() -> {
            try {
                terminalEditorContract(activity, input);
                systemImeShowGuard(activity, input);
                preImePlainKeys(activity, input);
                orderedPlainKeys(activity, input);
                unchangedTerminalFocus(activity, input);
                shiftedPlainKeys(activity, input);
                editorConnectionLifecycle(activity, input);
                terminalUnicodeCallbacks(activity, input);
            } catch (Exception error) { throw new AssertionError(error); }
        });
        waitForIdleSync();
    }

    @Override public void onStart() {
        Bundle result = new Bundle();
        try {
            FestermActivity activity = (FestermActivity) startActivitySync(
                new Intent(getTargetContext(), FestermActivity.class).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK));
            waitForIdleSync();
            Thread.sleep(2000);
            if (keyboardLifecycleValidation) {
                keyboardLifecycle(activity);
                result.putString("stream", "PASS: keyboard lifecycle only; no recording, secret-store or validation hooks\n");
                finish(Activity.RESULT_OK, result);
                return;
            }
            recordingEncoder(activity, recordingValidation);
            if (recordingValidation) providerRecording(activity);
            String reference = UUID.randomUUID().toString();
            byte[] fixture = "native-store-fixture".getBytes(StandardCharsets.UTF_8);
            require(activity.secretWrite(reference, fixture.clone(), false), "secret insert");
            require(!new File(activity.getNoBackupFilesDir(), "secrets/" + reference + ".reserved").exists(),
                "ordinary put acknowledgement leaves no reservation");
            require(Arrays.equals(activity.secretRead(reference), fixture), "secret round trip");
            require(!activity.secretWrite(reference, new byte[]{9}, false), "insert must not replace");
            require(Arrays.equals(activity.secretRead(reference), fixture), "insert collision preserves original bytes");
            // Both Rust put and create_reserved use this existing JNI entry
            // with replace=false; no new native test authority/API is added.
            String reservedReference = UUID.randomUUID().toString();
            byte[] reservedFixture = new byte[8193];
            Arrays.fill(reservedFixture, (byte) 37);
            require(activity.secretWrite(reservedReference, reservedFixture.clone(), false),
                "reserved reference shared native creation");
            require(!new File(activity.getNoBackupFilesDir(), "secrets/" + reservedReference + ".reserved").exists(),
                "reserved creation acknowledgement leaves no reservation");
            require(!activity.secretWrite(reservedReference, new byte[]{8}, false),
                "reserved reference cannot be replaced");
            require(Arrays.equals(activity.secretRead(reservedReference), reservedFixture),
                "reserved creation and collision preserve complete bytes");
            require(activity.secretDelete(reservedReference), "remove only owned reserved fixture");
            require(activity.secretWrite(reference, new byte[]{1, 2, 3}, true), "secret update");
            require(Arrays.equals(activity.secretRead(reference), new byte[]{1, 2, 3}), "updated value");
            File envelope = new File(activity.getNoBackupFilesDir(), "secrets/" + reference);
            require(envelope.isFile(), "ciphertext private storage");
            byte[] encrypted;
            try (FileInputStream source = new FileInputStream(envelope)) {
                encrypted = new byte[(int) envelope.length()];
                require(source.read(encrypted) == encrypted.length, "ciphertext read");
            }
            require(!Arrays.equals(encrypted, new byte[]{1, 2, 3}), "encrypted at rest");
            encrypted[encrypted.length - 1] ^= 1;
            try (FileOutputStream output = new FileOutputStream(envelope)) { output.write(encrypted); }
            boolean rejected = false;
            try { activity.secretRead(reference); } catch (java.security.GeneralSecurityException expected) { rejected = true; }
            require(rejected, "tampering rejected");
            require(activity.secretDelete(reference), "secret delete");
            require(activity.secretRead(reference) == null, "deleted reference missing");
            require(!activity.secretDelete(reference), "idempotent deletion");

            try (ParcelFileDescriptor root = ParcelFileDescriptor.adoptFd(activity.privateDirectoryDescriptor())) {
                android.system.StructStat pinned = Os.fstat(root.getFileDescriptor());
                android.system.StructStat expected = Os.stat(new File(activity.getFilesDir(), "festerm").getPath());
                require(pinned.st_uid == android.os.Process.myUid() && (pinned.st_mode & 0777) == 0700,
                    "native application root owner");
                require(pinned.st_dev == expected.st_dev && pinned.st_ino == expected.st_ino,
                    "native root keeps existing receipt location");
            }
            for (String suffix : new String[]{".bak", ".new", ".reserved"}) {
                String interruptedReference = UUID.randomUUID().toString();
                File interrupted = new File(activity.getNoBackupFilesDir(),
                    "secrets/" + interruptedReference + suffix);
                byte[] retained = new byte[]{8, 3, 5};
                java.nio.file.Files.createFile(interrupted.toPath(),
                    java.nio.file.attribute.PosixFilePermissions.asFileAttribute(
                        java.nio.file.attribute.PosixFilePermissions.fromString("rw-------")));
                try (FileOutputStream output = new FileOutputStream(interrupted)) {
                    output.write(retained);
                    output.getFD().sync();
                }
                android.system.StructStat owned = Os.lstat(interrupted.getPath());
                require(owned.st_uid == android.os.Process.myUid() && (owned.st_mode & 0777) == 0600,
                    "valid private interrupted fixture");
                boolean createRefused;
                try {
                    createRefused = !activity.secretWrite(interruptedReference, fixture.clone(), false);
                } catch (IOException expected) {
                    createRefused = true;
                }
                require(createRefused, "native create refuses interrupted material");
                boolean readRefused = false;
                try { activity.secretRead(interruptedReference); } catch (IOException expected) { readRefused = true; }
                require(readRefused, "interrupted native secret is not absence");
                boolean updateRefused = false;
                try { activity.secretWrite(interruptedReference, fixture.clone(), true); }
                catch (IOException expected) { updateRefused = true; }
                require(updateRefused, "interrupted native secret refuses update");
                boolean deleteRefused = false;
                try { activity.secretDelete(interruptedReference); }
                catch (IOException expected) { deleteRefused = true; }
                require(deleteRefused, "interrupted native secret refuses deletion");
                require(Arrays.equals(java.nio.file.Files.readAllBytes(interrupted.toPath()), retained),
                    "interrupted material preserved");
                require(!new File(activity.getNoBackupFilesDir(), "secrets/" + interruptedReference).exists(),
                    "no base created from interrupted state");
                require(interrupted.delete(), "remove only owned smoke fixture");
            }

            activity.copyText("native clipboard fixture");
            require("native clipboard fixture".equals(activity.clipboardText()), "clipboard round trip");
            activity.copyText("");
            onMain(() -> {
                activity.privacy(true, true, true);
                require((activity.getWindow().getAttributes().flags & WindowManager.LayoutParams.FLAG_SECURE) != 0,
                    "secure snapshot flag");
                activity.privacy(false, false, false);
            });

            Field picking = FestermActivity.class.getDeclaredField("picking");
            Field pickRequest = FestermActivity.class.getDeclaredField("PICK_DOCUMENT");
            Method pickResult = FestermActivity.class.getDeclaredMethod(
                "onActivityResult", int.class, int.class, Intent.class);
            picking.setAccessible(true);
            pickRequest.setAccessible(true);
            pickResult.setAccessible(true);
            int request = pickRequest.getInt(null);
            onMain(() -> {
                try {
                    picking.setBoolean(activity, true);
                    Intent ungranted = new Intent().setData(android.net.Uri.fromFile(
                        new File(activity.getFilesDir(), "private.txt")));
                    pickResult.invoke(activity, request, Activity.RESULT_OK, ungranted);
                    require(!picking.getBoolean(activity), "file URI refused before scheduling import");
                } catch (ReflectiveOperationException error) {
                    throw new AssertionError(error);
                }
            });

            // The actual JNI input connection must accept Unicode and composition.
            Field inputField = FestermActivity.class.getDeclaredField("input");
            inputField.setAccessible(true);
            View input = (View) inputField.get(activity);
            onMain(() -> keyboardImeOwnership(activity));
            onMain(() -> {
                try { terminalEditorContract(activity, input); systemImeShowGuard(activity, input); }
                catch (Exception error) { throw new AssertionError(error); }
            });
            postedAccentOwnershipTransition(activity, input);
            onMain(() -> {
                try { preImePlainKeys(activity, input); orderedPlainKeys(activity, input); }
                catch (Exception error) { throw new AssertionError(error); }
                activity.fieldDocument(1234, "a😀b", 2, 2, false, false);
                activity.keyboardMode(3);
                InputConnection connection = input.onCreateInputConnection(new EditorInfo());
                require(connection != null, "native input connection");
                require("a😀".contentEquals(connection.getTextBeforeCursor(20, 0)), "code point selection");
                require(connection.commitText("é", 1), "Unicode commit");
                activity.inputConsumed(0);
                activity.fieldDocument(1234, "stale", 5, 5, false, false);
                require("a😀é".contentEquals(connection.getTextBeforeCursor(20, 0)), "Unicode retained");
                require(connection.setComposingText("文", 1), "IME preedit");
                require(connection.commitText("文字", 1), "IME commit");
                require("a😀é文字".contentEquals(connection.getTextBeforeCursor(20, 0)), "composition replaces preedit");
                require(connection.deleteSurroundingTextInCodePoints(1, 0), "code point deletion");
                require("a😀é文".contentEquals(connection.getTextBeforeCursor(20, 0)), "deletion retained");
                require(connection.setSelection(0, 3), "Unicode range selection");
                require(connection.commitText("", 1), "selected text deletion");
                require("é文b".contentEquals(connection.getTextAfterCursor(20, 0)), "selected text removed");
                activity.fieldDocument(1235, "••", 2, 2, true, false);
                InputConnection secure = input.onCreateInputConnection(new EditorInfo());
                require(!connection.commitText("stale", 1), "old field input refused");
                CharSequence oldContext = connection.getTextBeforeCursor(20, 0);
                require(oldContext == null || oldContext.length() == 0, "old connection cannot read another field");
                require(secure.commitText("😀", 1), "secure Unicode input");
                require("•••".contentEquals(secure.getTextBeforeCursor(20, 0)), "secure context is masked");
                // The app-owned keyboard keeps the text-aware client for the same field.
                activity.keyboardMode(4);
                require(!secure.commitText("late", 1), "keyboard change retires the old connection");
                InputConnection custom = input.onCreateInputConnection(new EditorInfo());
                require(custom.commitText("😀", 1), "field-custom input reaches the field");
                activity.keyboardMode(0);
                require(!secure.commitText("late", 1), "hidden input refused");
                require(!custom.commitText("late", 1), "hidden field-custom input refused");
            });
            // New terminal JNI input must not precede the older synthetic privacy groups.
            onMain(() -> {
                try { unchangedTerminalFocus(activity, input); shiftedPlainKeys(activity, input);
                    editorConnectionLifecycle(activity, input);
                    terminalUnicodeCallbacks(activity, input); }
                catch (Exception error) { throw new AssertionError(error); }
            });
            waitForIdleSync();
            Class<?> verifier = Class.forName("org.rustls.platformverifier.CertificateVerifier");
            require(verifier != null, "native TLS verifier packaged");
            Class.forName("kotlin.enums.EnumEntriesKt");
            result.putString("stream", "PASS: H.264 recording"
                + (recordingValidation ? ", Rust JNI export and Files-provider publication" : "")
                + ", Keystore encryption/tamper/delete, clipboard, privacy, document URI, JNI IME, TLS classes\n");
            finish(Activity.RESULT_OK, result);
        } catch (Throwable error) {
            result.putString("stream", "FAIL: " + error.getClass().getName() + ": " + error.getMessage() + "\n");
            finish(Activity.RESULT_CANCELED, result);
        }
    }
}
