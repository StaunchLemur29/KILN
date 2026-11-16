KILN — analog video emulator

INSTALL
KILN runs on Apple Silicon Macs only (M1 or later), macOS 14 or newer. It does not run on Intel Macs.
Drag KILN into the Applications folder, then open it from there.
The first time, macOS says it can't check the app: click Done, open System Settings > Privacy & Security,
scroll down and choose Open Anyway next to KILN.

USE
- Drop a video or picture on the window, or press BROWSE.
- Pick a look from PRESETS, then fine-tune it with QUICK SHAPE or the detail controls.
- ADVANCED shows every module and knob; DRY / WET blends the effect with the clean picture.
- LIVE uses a camera, capture device, the screen, or a stream address as the source.
  The first time, macOS asks whether KILN may use the camera: choose Allow.
- RECORD records your knob moves while the video plays; EXPORT renders them into the file.
- EXPORT saves H.264, HEVC, ProRes 422 or ProRes 4444.
- Space plays and pauses. Click the round K badge for About.

RENDER WITHOUT OPENING THE WINDOW
  /Applications/KILN.app/Contents/MacOS/KILN --headless-export in.mp4 out.mov "PRESET=My Look" HI prores422
  After the two files: a preset name, SD / HI / SOURCE, DISPLAY=0-4, and h264 / hevc / prores422 / prores4444.

Your own presets and the last session are kept in
~/Library/Application Support/KILN

KILN includes FFmpeg (ffmpeg.org) for reading and writing video; its licence is inside the app
under Contents/Resources/Licenses.
