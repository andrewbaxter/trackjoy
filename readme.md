# Configuration

# Calibration

Trackjoy relies on device physical resolution information for making sure movements aren't warped as well as dealing with configuration in physical units (width, height) for portable configs.

My touchpad was 12x11cm but reported as 10.6x7.3cm and input was way off because of it.

I recommend you do `libinput measure touchpad-size` for every device you have, and if your device information is missing contribute it to the hardware database (the command will give you good instructions). You can use the output information locally until the change is merged and release upstream.

This fixed the input in my case.

You can use `jstest-gtk` to visualize and confirm your calibration.

# Automatic launching

Use the `trackjoy-juggler` command, with the same config you use for `trackjoy`:

```
# trackjoy-juggler config.json
```

This monitors `/dev/input/by-path` for close-by groups of trackpads and keyboards matching the configuration. When it finds groups, it'll launch `trackjoy` for them with the same config.

Note, this uses `udevadm` to check if a device is a trackpad (uses the `hid-multitouch` driver).

It relies on the path format being `PHYSPATH-TAG` where `PHYSPATH` ends with the USB path, and `TAG` is something like `kbd` or `event-kbd` or `mouse`, etc.

Both `udevadm` and `trackjoy` must be in your environment's `PATH`.
