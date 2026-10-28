# derisk's Agent (docs/agent.md), on the PC image, which is the desktop and
# tablet image alike. The Halium image goes without: it is one image for
# every Treble phone, and the Agent's window, three columns at least 900
# logical pixels wide, does not fit a screen under 600 on its short side.
#
# Sonne was here before it: the project's fork of Zed, with this agent
# inside it. It took about half a gigabyte of /usr for an editor the Text
# Editor and the Agent's own code view already cover, so it left the image
# and its agent moved into derisk; it is not coming back.
{ pkgs, ... }:

{
  # Its desktop entry claims the derisk-agent: scheme, which derisk's
  # palette opens the Agent with ("Ask the agent").
  environment.systemPackages = [ pkgs.derisk.agent ];

  # Sonne also claimed text and source files; derisk's Text Editor does now
  # (desktop.nix).
}
