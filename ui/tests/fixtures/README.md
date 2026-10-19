# Popover state fixture

state.json records the shape of rotator-core's serialized Snapshot and Settings
with deterministic synthetic photo metadata for two displays. No real keys or
hosted images are used. popover.spec.cjs installs the fake Tauri object before
app.js loads, records every invoke call, and serves the bundled blue PNG through
convertFileSrc. The real-app suite separately exercises the actual IPC and worker.
