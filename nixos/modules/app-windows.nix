# app-windows on every target (docs/app-windows.md): the client that shows
# a guest system's apps in the app grid and its windows as windows here.
# Nothing starts it yet; a guest's own module does, with that guest's
# server and token.
{ pkgs, ... }:

{
  environment.systemPackages = [ pkgs.app-windows ];
}
