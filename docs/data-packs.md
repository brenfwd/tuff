# Data packs

A world loads its own data packs from `datapacks/`. Functions, tags,
recipes, loot tables, predicates and advancements are applied; other
content, such as worldgen, is not. With the server stopped, the `datapack`
subcommand adds, lists and removes packs for a world folder or a server
file alike:

    tuff datapack add <world> <pack.zip|folder> [--name <name>] [--enable] [--replace]
    tuff datapack list <world>
    tuff datapack remove <world> <name>

`<world>` is a world folder holding `level.dat` (for example `./world`) or
a server file (for example `./world.mcserver`). The command locks the
world, so it refuses a world a server is running. `add` checks the pack (a
`pack.mcmeta` at its root, at most 16 MiB per file, 256 MiB in total and
65,535 entries, no paths that leave the pack) before storing it as
`datapacks/<name>.zip` or `datapacks/<name>/`. A name is up to 64 ASCII
letters, digits, `_`, `-`, `.` or `+`; without `--name` it comes from the
file or folder name. An existing pack is refused unless `--replace` is
given. `add` and `list` note a pack made for another game version.

New packs load on the next start; `--enable` also re-enables a pack the
world's saved selection had disabled. `remove` deletes the pack and leaves
the saved selection alone; the server drops the missing pack on its next
start. `tuff datapack help` prints the details. While the server runs, the
panel's Datapacks page and the `/datapack` command manage packs.

Without the subcommand, copy packs into `<world>/datapacks/` for a folder
world. A server file keeps them in its `sqlar` table under
`extra/datapacks/`, which the `sqlite3` shell can update while the server
is stopped:

    mkdir -p stage/extra/datapacks && cp pack.zip stage/extra/datapacks/
    sqlite3 world.mcserver -A -u -C stage extra/datapacks/pack.zip
