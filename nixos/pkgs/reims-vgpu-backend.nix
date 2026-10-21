# Reims' GPU backend for arm64 macOS guests (docs/macos-arm64.md, "A GPU"):
# the process behind qemu-vmapple's apv-gpu-proxy device. It decodes the
# command stream of macOS's own paravirtual GPU driver
# (AppleParavirtGPU.kext, already in every macOS) and runs the guest's
# Metal on the host's Vulkan, through metal2vulkan. Nothing is installed in
# the guest. aquarat/reims-vgpu, steelbrain's Reims as aquarat runs it in
# production on Asahi Linux, from a pinned commit.
{
  lib,
  rustPlatform,
  fetchFromGitHub,
  makeWrapper,
  spirv-tools,
  vulkan-loader,
}:

rustPlatform.buildRustPackage {
  pname = "reims-vgpu-backend";
  version = "0-unstable-2026-10-19";

  src = fetchFromGitHub {
    owner = "aquarat";
    repo = "reims-vgpu";
    rev = "2677a20ca8a34ccbe33b7a579ce493723fbc824e";
    hash = "sha256-pBF5X9yyKu709gFs6a6cg+yarILUc6depsFjlIW8GYw=";
  };

  # The commit's own Cargo.lock, copied beside this file so metal2vulkan's
  # git pin can carry its hash without importing from the fetched source.
  cargoLock = {
    lockFile = ./reims-vgpu-backend.Cargo.lock;
    outputHashes."metal2vulkan-0.1.0" = "sha256-100o4dls63KpuRFR1R6paFocSPMoZNYeIvAJEPjl3hA=";
  };
  cargoBuildFlags = [
    "-p"
    "reims-vgpu-backend"
  ];

  nativeBuildInputs = [ makeWrapper ];

  # The Vulkan suites need a GPU, or lavapipe, which the build sandbox lacks.
  doCheck = false;

  # ash opens libvulkan.so.1 at run time, so the loader goes on the
  # binary's search path; the driver is the host's (VK_DRIVER_FILES).
  # metal2vulkan checks every SPIR-V module it emits with spirv-val, and
  # without it on PATH every shader translation fails.
  postFixup = ''
    patchelf --add-rpath ${lib.makeLibraryPath [ vulkan-loader ]} \
      $out/bin/reims-vgpu-backend
    wrapProgram $out/bin/reims-vgpu-backend \
      --prefix PATH : ${lib.makeBinPath [ spirv-tools ]}
  '';

  meta = {
    description = "Out-of-process Reims paravirtual GPU for vmapple macOS guests";
    homepage = "https://github.com/aquarat/reims-vgpu";
    license = lib.licenses.lgpl3Plus;
    platforms = lib.platforms.linux;
    mainProgram = "reims-vgpu-backend";
  };
}
