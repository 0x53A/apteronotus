{ pkgs ? import <nixpkgs> { } }:

let
  # eframe dlopens GL and windowing at runtime; cpal needs alsa.
  libPath = with pkgs; lib.makeLibraryPath [
    alsa-lib
    libGL
    libxkbcommon
    wayland
    xorg.libX11
    xorg.libXcursor
    xorg.libXi
    xorg.libXrandr
  ];
in
pkgs.mkShell {
  buildInputs = with pkgs; [
    pkg-config
    alsa-lib
    udev
  ];

  LD_LIBRARY_PATH = libPath;
}
