# The memory allocator

On a PC every dynamically linked process allocates through GrapheneOS's
[hardened_malloc](https://github.com/GrapheneOS/hardened_malloc). It is
nixpkgs' `graphene-hardened-malloc` in its default configuration, not the
light one. `nixos/modules/allocator.nix` names it in
`/etc/ld-nix.so.preload`, which nixpkgs' glibc reads in place of
`/etc/ld.so.preload`. derisk is built with its `hardened-malloc` feature,
which makes mcsapi's `mcsapi-hardened-malloc` crate its Rust global
allocator. That crate uses the same `libhardened_malloc.so`, mapped once.
Rust's frees go through `free_sized`, so freeing with a wrong length aborts.

The module writes the preload itself instead of setting NixOS's
`environment.memoryAllocator.provider`, because that option preloads a copy
of the library from another store path. The library has no `DT_SONAME`, so
`ld.so` would map that copy and derisk's as two separate allocators. Halium
does not import the module yet, and [What is not done](not-done.md) says why.

[Danube](danube.md)'s WebKit keeps its own allocator, libpas, with its
Gigacage, for JavaScript and DOM objects. Everything else it allocates goes
through `malloc`, so it runs under the preload like any other program.
Uranium, the Chromium build Danube replaced, could not, because
PartitionAlloc replaces `malloc` itself. It ran without the preload, and it
is gone.
