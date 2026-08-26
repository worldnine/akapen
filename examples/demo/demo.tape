# demo.tape — fully automated akapen screencast (VHS)
#
#   vhs examples/demo/demo.tape        # from the repo root
#
# Drives the fake-agent demo end to end, two review rounds deep:
# comment → send → ⚡ → reload → walk the marks → comment again →
# second revision → time-travel through four generations (v0 outline,
# v1 draft, v2, v3). Deterministic; re-render any time.
# Requires: vhs (brew install vhs), target/release/akapen built.

Output examples/demo/out/demo.gif
Output examples/demo/out/demo.mp4
Output examples/demo/out/demo.webm

Set Shell bash
Set Framerate 50
Set FontFamily "Guguru Sans Code"
Set FontSize 18
Set Width 1280
Set Height 800
Set Padding 12
Set TypingSpeed 40ms

Env AKAPEN_OPTS "--ime off"

# --- setup (hidden): scratch dir + akapen launch --------------------------
Hide
Type "examples/demo/run.sh /tmp/akapen-vhs" Enter
Wait+Screen@15s /driftwatch/
Sleep 1s
Show

# --- skim the rendered document --------------------------------------------
Sleep 2s
Type "jjjjjjjj"
Sleep 1.5s

# --- round 1, comment 1: §3 polling -----------------------------------------
Type "jjjjjjjjjjjj"
Sleep 1s
Type "v"
Sleep 500ms
Type "jjj"
Sleep 500ms
Type "c"
Sleep 800ms
Type@30ms "Polling per file won't scale — go event-driven (FSEvents/inotify); keep polling only as a fallback."
Sleep 800ms
Enter
Sleep 1.5s

# --- round 1, comment 2: §5 tone, then send ---------------------------------
Type "G"
Sleep 800ms
Type "kkkkkkkkkk"
Sleep 800ms
Type "v"
Sleep 400ms
Type "jjjj"
Sleep 400ms
Type "c"
Sleep 800ms
Type@30ms "Delete this section. This is a design doc, not a landing page."
Sleep 800ms
Enter
Sleep 1s
Type "s"
Sleep 1s

# --- the agent revises; ⚡ lights up; reload ---------------------------------
Wait+Screen@20s /⚡/
Sleep 1.5s
Type "r"
Sleep 2s

# --- walk the differences ----------------------------------------------------
Type "n"
Sleep 1.5s
Type "n"
Sleep 1.5s
Type "n"
Sleep 1.5s
Type "N"
Sleep 1.5s

# --- Tab: source mode, the same marks as a diff ------------------------------
# n/N leaves a difference selected; Esc first. Source mode shows the
# baseline diff inline — deleted lines as red ▌ pairs, line numbers,
# syntax highlighting.
Escape
Sleep 400ms
Tab
Sleep 2.5s
Type "jjjj"
Sleep 2s
Tab
Sleep 1s

# --- round 2: the roadmap still ships auto-fix -------------------------------
Type "G"
Sleep 800ms
Type "k"
Sleep 500ms
Type "v"
Sleep 400ms
Type "c"
Sleep 800ms
Type@30ms "Roadmap still ships auto-fix by default — contradicts the read-only rule in §1. Replace it with the watch mode."
Sleep 800ms
Enter
Sleep 1s
Type "s"
Sleep 1s

Wait+Screen@20s /⚡/
Sleep 1.5s
Type "r"
Sleep 2s

# --- the gradebook: four generations, one axis -------------------------------
Left
Sleep 1.8s
Left
Sleep 1.8s
Left
Sleep 2.2s
Right
Sleep 1.2s
Right
Sleep 1.2s
Right
Sleep 2.5s

Hide
Type "q"
