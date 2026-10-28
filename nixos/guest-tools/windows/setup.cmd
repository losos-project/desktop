@echo off
rem What a Windows machine takes from the LosOS Windows tools CD at the first
rem sign-in after Setup (autounattend.xml's FirstLogonCommands). Setup itself
rem only loaded the disk and network drivers; this installs the rest.

rem This CD's own drive, whatever letter Windows gave it.
set tools=%~d0

rem virtio-win's guest tools: every virtio driver (the display, the balloon,
rem the tablet, the random number generator), the QEMU guest agent, which
rem is how the host's desktop sees the machine's address and shuts it down,
rem and the SPICE agent, which shares the desktop's clipboard with the
rem machine's screen while its viewer has the keyboard.
"%tools%\virtio-win-guest-tools.exe" /install /passive /norestart
