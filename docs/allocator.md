# The memory allocator

Every dynamically linked process allocates through GrapheneOS's
[hardened_malloc](https://github.com/GrapheneOS/hardened_malloc). On a PC it
is nixpkgs' `graphene-hardened-malloc` in its default configuration, not the
light one. `nixos/modules/allocator.nix` names it in
`/etc/ld-nix.so.preload`, which nixpkgs' glibc reads in place of
`/etc/ld.so.preload`. derisk is built with its `hardened-malloc` feature,
which makes mcsapi's `mcsapi-hardened-malloc` crate its Rust global
allocator. That crate uses the same `libhardened_malloc.so`, mapped once.
Rust's frees go through `free_sized`, so freeing with a wrong length aborts.

The module writes the preload itself instead of setting NixOS's
`environment.memoryAllocator.provider`, because that option preloads a copy
of the library from another store path. The library has no `DT_SONAME`, so
`ld.so` would map that copy and derisk's as two separate allocators.

## On a phone

The [Halium GSI](halium.md) boots the phone's own kernel, so the build
cannot know two things hardened_malloc depends on.

- **The address space.** The default configuration reserves 32 GiB per
  size class in each of 4 arenas as it starts, about 12 TiB, which needs a
  48-bit address space. Most Android kernels give a process 39 bits, 512 GiB,
  and every process would abort on its first allocation. The GSI's build
  sets `CONFIG_CLASS_REGION_SIZE` to 512 MiB and `CONFIG_N_ARENA` to 1, which
  reserves 49 GiB. One size class can still hold 512 MiB of a process's
  small allocations. Anything over 128 KiB is mapped on its own.
- **The page size.** hardened_malloc is built for 4 KiB pages, and a few
  newer phones run 16 KiB ones.

So the initrd tries it first. `losos-allocator-trial` runs one program with
it preloaded, and writes `/run/losos/ld-nix.so.preload` naming the library
only if that program ran. `/etc/ld-nix.so.preload` links there, so the
system's first process after the switch from the initrd, and every one
after it, gets hardened_malloc, or none of them do. derisk is not built
against it on the GSI, because a linked copy would stop the desktop on a
phone where the trial failed. It gets hardened_malloc through the preload
like every other program, without the length check on its frees.

> [!NOTE]
> Android's own processes, the HALs in the LXC container, use bionic and its
> allocator. The preload is glibc's and does not reach them.

[Danube](danube.md)'s WebKit keeps its own allocator, libpas, with its
Gigacage, for JavaScript and DOM objects. Everything else it allocates goes
through `malloc`, so it runs under the preload like any other program.
Uranium, the Chromium build Danube replaced, could not, because
PartitionAlloc replaces `malloc` itself. It ran without the preload, and it
is gone.
