//! `sqlite3 world.mcserver` then `.load ./libtuff_sqlite` registers tuff's
//! NBT functions and temporary views on that connection.

use std::os::raw::{c_char, c_int};

use rusqlite::{Connection, ffi};

/// Entry point SQLite derives from the library name `libtuff_sqlite`.
///
/// # Safety
/// Called by SQLite with a valid connection and API routine table.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sqlite3_tuffsqlite_init(
    db: *mut ffi::sqlite3,
    pz_err_msg: *mut *mut c_char,
    p_api: *mut ffi::sqlite3_api_routines,
) -> c_int {
    unsafe { Connection::extension_init2(db, pz_err_msg, p_api, init) }
}

fn init(conn: Connection) -> rusqlite::Result<bool> {
    tuff_sqlite_nbt::register_functions(&conn)?;
    let _ = tuff_sqlite_nbt::create_views(&conn);
    Ok(false)
}
