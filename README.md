# alarm-clock

A Raspberry Pi alarm clock appliance with a hardware backlight display.

## Sysfs Backlight

The application discovers the backlight device at boot by globbing
`/sys/class/backlight/*/` and reading `max_brightness` from the first
matching device. If no device is found, a no-op controller is used (all
brightness/bl_power writes are silently dropped and logged).

Expected backlight paths:
- Official Raspberry Pi 7" touchscreen: `/sys/class/backlight/10-0045/`
- Other displays: check `/sys/class/backlight/` for available devices

The controller writes two sysfs attributes:
- `brightness`: integer 0..max_brightness (scaled from percentage)
- `bl_power`: 0 (on) / 1 (off)

`bl_power` is used only for power state transitions (bedtime off, wake-on-touch).
Brightness modulation (strobe, dynamic, override) uses `brightness` only.

## RTC / fake-hwclock

The Pi has no RTC battery. On boot, `fake-hwclock` restores the last-shutdown
system time from the filesystem. The alarm clock uses `chrono::Local::now()`
for all time computations, so the system clock must be reasonably accurate
for the scheduler to work.

Without NTP or a connected RTC, time drift accumulates across reboots.
Consider installing `fake-hwclock` (pre-installed on Raspberry Pi OS) and
enabling NTP via `timedatectl set-ntp true`.