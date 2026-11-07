#!/usr/bin/env python3
"""Drives the running app through its test hooks (see src/testhooks.h) and checks
what is on screen: the song sheet, the queue beside the visual, the current line
in the player and swiping on the mini player.

usage: ui_test.py <path to the app>

Needs a display and OpenGL (software is fine). Every run uses a new profile and
its own small songs, and nothing here touches the network.
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import wave

APP = sys.argv[1]
ROOT = tempfile.mkdtemp(prefix="beatlumen-ui-")
MUSIC = os.path.join(ROOT, "music")
DUMP = os.path.join(ROOT, "dump.json")
LOG = os.path.join(ROOT, "app.log")
TITLES = ["Alpha", "Bravo", "Charlie", "Delta"]
LINES = {2: "Line at two", 10: "Line at ten", 30: "Line at thirty"}


def make_music():
    os.makedirs(MUSIC)
    for title in TITLES:
        with wave.open(os.path.join(MUSIC, title + ".wav"), "wb") as w:
            w.setnchannels(1)
            w.setsampwidth(2)
            w.setframerate(8000)
            w.writeframes(b"\0\0" * 8000 * 60)
    with open(os.path.join(MUSIC, "Alpha.lrc"), "w", encoding="utf8") as f:
        for t, text in LINES.items():
            f.write(f"[00:{t:02d}.00]{text}\n")


def song_id(title):
    return "file:" + os.path.join(MUSIC, title + ".wav")


class App:
    def __init__(self, size, phone):
        self.width, self.height = size
        self.phone = phone
        env = dict(os.environ)
        env.update(
            LC_ALL="C.UTF-8",
            LANG="C.UTF-8",
            LANGUAGE="en",
            QT_FORCE_STDERR_LOGGING="1",
            XDG_CONFIG_HOME=os.path.join(ROOT, "config"),
            XDG_DATA_HOME=os.path.join(ROOT, "data"),
            XDG_CACHE_HOME=os.path.join(ROOT, "cache"),
            BEATLUMEN_TEST_DUMP=DUMP,
            BEATLUMEN_TEST_SIZE=f"{self.width}x{self.height}",
        )
        env.pop("BEATLUMEN_TEST_MOBILE", None)
        if phone:
            env["BEATLUMEN_TEST_MOBILE"] = "1"
        for path in (DUMP, DUMP + ".cmd"):
            if os.path.exists(path):
                os.remove(path)
        self.log = open(LOG, "a", encoding="utf8")
        self.process = subprocess.Popen([APP], env=env, stdout=self.log, stderr=self.log)
        self.wait("the window", lambda: self.items())

    def send(self, line):
        tmp = DUMP + ".cmd.tmp"
        with open(tmp, "w", encoding="utf8") as f:
            f.write(line + "\n")
        os.rename(tmp, DUMP + ".cmd")
        end = time.time() + 30
        while os.path.exists(DUMP + ".cmd"):
            if time.time() > end or self.process.poll() is not None:
                raise SystemExit(f"the app does not answer {line!r}")
            time.sleep(0.02)

    def core(self, command, **args):
        self.send(f"core {command} {json.dumps(args)}")

    def items(self):
        # A grab lays everything out, also where the display draws no frames.
        self.send("shot " + os.path.join(ROOT, "shot.png"))
        for _ in range(20):
            try:
                with open(DUMP, encoding="utf8") as f:
                    return json.load(f)
            except (OSError, ValueError):
                time.sleep(0.05)
        return []

    def wait(self, what, check, seconds=30):
        end = time.time() + seconds
        while time.time() < end:
            found = check()
            if found:
                return found
            time.sleep(0.2)
        names = sorted({i["text"] or i.get("a11yName", "") for i in self.items()})
        raise SystemExit(f"timed out waiting for {what}; on screen: {names}")

    def labels(self, text):
        return [i for i in self.items() if i["text"] == text]

    def shows(self, text):
        return self.wait(f"{text!r} on screen", lambda: self.labels(text))

    def gone(self, text):
        self.wait(f"{text!r} to go away", lambda: not self.labels(text))

    def named(self, name, buttons=True):
        """Controls with this name for a screen reader, the last one first: it is on top."""
        hits = [i for i in self.items() if i.get("a11yName") == name and i.get("enabled")]
        if buttons:
            hits = [i for i in hits if i.get("button")] or hits
        return list(reversed(hits))

    def click(self, item):
        self.send(f"click {item['x'] + item['w'] / 2} {item['y'] + item['h'] / 2}")

    def click_named(self, name):
        # While music plays over the visual, the controls fade away after a while.
        self.send("hover 300 300")
        self.send("hover 302 302")
        hits = self.wait(f"a control named {name!r}", lambda: self.named(name))
        self.click(hits[0])

    def click_text(self, text):
        hits = self.shows(text)
        self.click(hits[-1])

    def key(self, key):
        self.send(f"key {key}")

    def set(self, name, value):
        self.send(f"set {name} {json.dumps(value)}")

    def quit(self):
        # Closing a phone's window goes back a page instead of ending the app.
        if self.phone:
            self.process.terminate()
        else:
            self.send("quit")
        try:
            self.process.wait(20)
        except subprocess.TimeoutExpired:
            self.process.kill()
            raise SystemExit("the app did not end")
        self.log.close()
        if not self.phone and self.process.returncode != 0:
            raise SystemExit(f"the app ended with {self.process.returncode}")


def start(size, phone):
    app = App(size, phone)
    app.core("settings.set", key="ui.setupDone", value=True)
    app.core("settings.set", key="libraryFolders", value=[MUSIC])
    app.core("library.rescan")
    app.core("play", ids=[song_id(t) for t in TITLES], start=0)
    # A new profile may already have the setup open.
    time.sleep(1.5)
    skip = app.named("Skip setup")
    if skip:
        app.click(skip[0])
        app.gone("Skip setup")
    return app


def phone_song_sheet():
    app = start((390, 844), True)
    app.set("page", 0)
    # The first row's menu: a sheet with the song on top and big rows.
    app.shows("Alpha")
    row = app.wait("a song list", lambda: [i for i in app.items() if i.get("a11yName") == "More" and "IconButton" in i["type"]])
    app.click(row[0])
    app.shows("Play next")
    add = app.labels("Add to queue") or app.named("Add to queue")
    assert add, "no row to add to the queue"
    assert add[0]["h"] >= 40, f"rows are for fingers: {add[0]}"
    assert add[0]["w"] >= 300, f"rows span the sheet: {add[0]}"
    # Back closes it.
    app.key("Escape")
    app.gone("Play next")
    # An action closes it too.
    app.click(row[0])
    app.shows("Play next")
    app.click_named("Play next")
    app.gone("Play next")
    app.quit()
    print("ok: song sheet")


def phone_queue_and_swipe():
    app = start((390, 844), True)
    app.set("page", 5)
    # Swiping on the mini player.
    title = app.wait("the mini player", lambda: [i for i in app.labels("Alpha") if i["y"] > 600])[0]
    y = title["y"] + title["h"] / 2
    app.send(f"drag 200 {y} 170 {y}")
    time.sleep(0.5)
    assert [i for i in app.labels("Alpha") if i["y"] > 600], "a short drag does nothing"
    bar = lambda t: [i for i in app.labels(t) if i["y"] > 600]
    app.send(f"drag 270 {y} 30 {y}")
    app.wait("the next song", lambda: bar("Bravo"))
    app.core("seek", position=0)  # Later in a song, "previous" restarts it.
    app.send(f"drag 30 {y} 270 {y}")
    app.wait("the song before", lambda: bar("Alpha"))

    # The current line, in place of the artist.
    app.core("seek", position=10.5)
    line = app.shows("Line at ten")
    assert line[0]["y"] > title["y"], "under the title"
    app.core("seek", position=31)
    app.shows("Line at thirty")
    # It can be switched off.
    app.core("settings.set", key="ui.barLyrics", value=False)
    app.gone("Line at thirty")
    app.core("settings.set", key="ui.barLyrics", value=True)
    app.shows("Line at thirty")

    # The queue as a sheet: the visual stays in sight above it.
    app.set("page", -1)
    app.click_named("Queue")
    app.shows("Queue")
    time.sleep(0.6)  # It slides up.
    heading = app.shows("Queue")
    assert heading[-1]["y"] > 844 * 0.2, f"the upper part stays free: {heading[-1]}"
    app.shows("Bravo")
    # Pulling the handle down closes it.
    grip_y = heading[-1]["y"] - 16
    app.send(f"drag 195 {grip_y} 195 {grip_y + 300}")
    app.gone("Clear queue")
    # And tapping above it.
    app.click_named("Queue")
    app.shows("Clear queue")
    time.sleep(0.6)
    app.send("click 195 40")
    app.gone("Clear queue")
    app.quit()
    print("ok: mini player and queue sheet")


def desktop():
    app = start((1280, 800), False)
    app.set("page", -1)
    # The queue as a panel: the title over the visual stays free.
    app.click_named("Queue")
    heading = app.wait("the panel", lambda: [i for i in app.labels("Queue") if i["w"] < 300])
    assert heading[-1]["x"] > 1280 * 0.5, f"at the side: {heading[-1]}"
    titles = [i for i in app.labels("Alpha") if i["x"] + i["w"] <= heading[-1]["x"]]
    assert titles, "the song's title stays beside the panel"
    # Escape closes it, and the button. Shortcuts need an active window, which
    # a virtual display without a window manager does not have.
    if not os.environ.get("UI_TEST_NO_SHORTCUTS"):
        app.key("Escape")
        app.wait("the panel to close", lambda: not [i for i in app.labels("Queue") if i["x"] > 640])
        app.click_named("Queue")
        app.shows("Clear queue")
    app.click_named("Close")
    app.wait("the panel to close", lambda: not [i for i in app.labels("Queue") if i["x"] > 640])
    # Beside a page, too.
    app.set("page", 0)
    app.click_named("Queue")
    panel = app.wait("the panel", lambda: [i for i in app.labels("Queue") if i["x"] > 640])
    left = [i for i in app.items() if i["text"] in ("Library",) and i["x"] < panel[0]["x"]]
    assert left, "the page stays beside it"
    app.click_named("Close")
    # A computer keeps its menu: no sheet.
    row = app.wait("a song list", lambda: [i for i in app.items() if i.get("a11yName") == "More" and "IconButton" in i["type"]])
    app.click(row[0])
    item = app.shows("Play next")
    assert item[0]["h"] < 60 and item[0]["w"] <= 300, f"a menu: {item[0]}"
    app.key("Escape")
    app.quit()
    print("ok: side panel and menu on a computer")


def check_log():
    allowed = ("Accessible attached property",)
    with open(LOG, encoding="utf8") as f:
        bad = [
            line.strip()
            for line in f
            if (".qml:" in line or "TypeError" in line or "ReferenceError" in line)
            and not any(a in line for a in allowed)
        ]
    if bad:
        raise SystemExit("QML errors:\n" + "\n".join(bad))


def main():
    make_music()
    try:
        phone_song_sheet()
        phone_queue_and_swipe()
        desktop()
        check_log()
    except (SystemExit, AssertionError, Exception):
        if os.path.exists(LOG):
            with open(LOG, encoding="utf8") as f:
                print(f.read()[-4000:], file=sys.stderr)
        raise
    finally:
        shutil.rmtree(ROOT, ignore_errors=True)
    print("all passed")


main()
