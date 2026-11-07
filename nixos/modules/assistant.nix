# Ask derisk's language model (docs/assistant.md): Ollama on this computer,
# which the palette hands the requests its own rules don't follow, and
# which downloads the model the first time one does. No model is in the
# image: the default is 2.5 GB and the /usr slot has 8 GB for everything.
#
# derisk talks to it on 127.0.0.1:11434, the default in Settings ›
# Assistant, where a person can point it at another Ollama-compatible
# server instead.
{ pkgs, ... }:
{
  services.ollama = {
    enable = true;
    # The Vulkan build runs the model on a PC's GPU when it has one (Intel,
    # AMD, NVIDIA's open module), several times as fast as its CPU, and on
    # the CPU otherwise, for 45 MiB more. A phone's GPU is Android's, behind
    # libhybris, which no Vulkan loader here reaches.
    package = if pkgs.stdenv.hostPlatform.isx86_64 then pkgs.ollama-vulkan else pkgs.ollama;
    environmentVariables = {
      # The OpenAI wire has no num_ctx; the old backend passed 8192 because
      # the tools and a busy desktop's description take ~3000 tokens and
      # Ollama's default (4096) would cut a long request short.
      OLLAMA_CONTEXT_LENGTH = "8192";
    };
    # NixOS's defaults otherwise: loopback only, a dynamic user, the models
    # in /var/lib/ollama on the root partition, where state lives.
  };
}
