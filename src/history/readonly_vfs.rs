//! A non-default SQLite VFS shim for registered legacy sources.
//!
//! READONLY on the main database alone is insufficient: SQLite opens the WAL
//! with CREATE and the Unix VFS normally maps SHM writable. This shim forces
//! read-only opens, rejects writes/deletes/truncation, and only accepts read-only
//! SHM mappings. The generated URI additionally sets readonly_shm=1. Locks and
//! SQLite's WAL recovery/consistency checks remain intact (never immutable).

use rusqlite::ffi::*;
use std::ffi::{c_char, c_int, c_void};
use std::ptr;
use std::sync::OnceLock;

pub(super) const NAME: &str = "codex-history-readonly-v1";
static REGISTERED: OnceLock<Result<(), c_int>> = OnceLock::new();
static BASE: OnceLock<usize> = OnceLock::new();

#[repr(C)]
struct File {
    header: sqlite3_file,
    real: *mut sqlite3_file,
}

pub(super) fn register() -> Result<(), c_int> {
    *REGISTERED.get_or_init(|| {
        // SAFETY: SQLite initialization/registration is serialized here. The
        // registered VFS and default VFS live until process exit; never mutate
        // the default VFS. Each xOpen owns its own sqlite3_malloc allocation.
        unsafe {
            sqlite3_initialize();
            let base = sqlite3_vfs_find(ptr::null());
            if base.is_null() {
                return Err(SQLITE_CANTOPEN);
            }
            BASE.set(base as usize).map_err(|_| SQLITE_MISUSE)?;
            let mut vfs = Box::new(*base);
            vfs.zName = c"codex-history-readonly-v1".as_ptr();
            vfs.pNext = ptr::null_mut();
            vfs.szOsFile = size_of::<File>() as c_int;
            vfs.xOpen = Some(open);
            vfs.xDelete = Some(delete);
            let pointer = Box::into_raw(vfs);
            let rc = sqlite3_vfs_register(pointer, 0);
            if rc == SQLITE_OK {
                Ok(())
            } else {
                drop(Box::from_raw(pointer));
                Err(rc)
            }
        }
    })
}

// SAFETY throughout the callbacks: SQLite supplies live file/output pointers
// according to its VFS ABI. Only successfully opened files receive METHODS.
// No callback panics, retains borrowed buffers, or exposes the underlying file.
unsafe extern "C" fn open(
    _: *mut sqlite3_vfs,
    name: *const c_char,
    file: *mut sqlite3_file,
    flags: c_int,
    out_flags: *mut c_int,
) -> c_int {
    unsafe {
        (*file).pMethods = ptr::null();
        let Some(&base) = BASE.get() else {
            return SQLITE_CANTOPEN;
        };
        let base = base as *mut sqlite3_vfs;
        if name.is_null()
            || flags & (SQLITE_OPEN_MAIN_DB | SQLITE_OPEN_WAL | SQLITE_OPEN_MAIN_JOURNAL) == 0
        {
            return SQLITE_READONLY;
        }
        if flags & SQLITE_OPEN_MAIN_DB != 0
            && sqlite3_uri_boolean(name, c"readonly_shm".as_ptr(), 0) != 1
        {
            return SQLITE_READONLY;
        }
        let real = sqlite3_malloc((*base).szOsFile) as *mut sqlite3_file;
        if real.is_null() {
            return SQLITE_NOMEM;
        }
        ptr::write_bytes(real.cast::<u8>(), 0, (*base).szOsFile as usize);
        let flags = (flags
            & !(SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE | SQLITE_OPEN_DELETEONCLOSE))
            | SQLITE_OPEN_READONLY
            | SQLITE_OPEN_NOFOLLOW;
        let rc = ((*base).xOpen.unwrap())(base, name, real, flags, out_flags);
        if rc != SQLITE_OK {
            if !(*real).pMethods.is_null() {
                ((*(*real).pMethods).xClose.unwrap())(real);
            }
            sqlite3_free(real.cast());
            return rc;
        }
        ptr::write(
            file.cast::<File>(),
            File {
                header: sqlite3_file { pMethods: &METHODS },
                real,
            },
        );
        SQLITE_OK
    }
}
unsafe fn real(f: *mut sqlite3_file) -> *mut sqlite3_file {
    unsafe { (*f.cast::<File>()).real }
}
unsafe fn methods(f: *mut sqlite3_file) -> &'static sqlite3_io_methods {
    unsafe { &*(*real(f)).pMethods }
}
unsafe extern "C" fn close(f: *mut sqlite3_file) -> c_int {
    unsafe {
        let rc = (methods(f).xClose.unwrap())(real(f));
        sqlite3_free(real(f).cast());
        (*f).pMethods = ptr::null();
        rc
    }
}
unsafe extern "C" fn read(f: *mut sqlite3_file, b: *mut c_void, n: c_int, o: i64) -> c_int {
    unsafe { (methods(f).xRead.unwrap())(real(f), b, n, o) }
}
unsafe extern "C" fn write(_: *mut sqlite3_file, _: *const c_void, _: c_int, _: i64) -> c_int {
    SQLITE_READONLY
}
unsafe extern "C" fn truncate(_: *mut sqlite3_file, _: i64) -> c_int {
    SQLITE_READONLY
}
unsafe extern "C" fn sync(_: *mut sqlite3_file, _: c_int) -> c_int {
    SQLITE_OK
}
unsafe extern "C" fn size(f: *mut sqlite3_file, n: *mut i64) -> c_int {
    unsafe { (methods(f).xFileSize.unwrap())(real(f), n) }
}
unsafe extern "C" fn lock(f: *mut sqlite3_file, level: c_int) -> c_int {
    unsafe { (methods(f).xLock.unwrap())(real(f), level) }
}
unsafe extern "C" fn unlock(f: *mut sqlite3_file, level: c_int) -> c_int {
    unsafe { (methods(f).xUnlock.unwrap())(real(f), level) }
}
unsafe extern "C" fn reserved(f: *mut sqlite3_file, out: *mut c_int) -> c_int {
    unsafe { (methods(f).xCheckReservedLock.unwrap())(real(f), out) }
}
unsafe extern "C" fn control(f: *mut sqlite3_file, op: c_int, arg: *mut c_void) -> c_int {
    // Do not let file controls resize files or expose the underlying methods.
    if !matches!(
        op,
        SQLITE_FCNTL_HAS_MOVED | SQLITE_FCNTL_LOCKSTATE | SQLITE_FCNTL_VFSNAME
    ) {
        return SQLITE_NOTFOUND;
    }
    unsafe { (methods(f).xFileControl.unwrap())(real(f), op, arg) }
}
unsafe extern "C" fn sector(f: *mut sqlite3_file) -> c_int {
    unsafe { (methods(f).xSectorSize.unwrap())(real(f)) }
}
unsafe extern "C" fn device(f: *mut sqlite3_file) -> c_int {
    unsafe { (methods(f).xDeviceCharacteristics.unwrap())(real(f)) & !SQLITE_IOCAP_IMMUTABLE }
}
unsafe extern "C" fn shm_map(
    f: *mut sqlite3_file,
    page: c_int,
    size: c_int,
    _: c_int,
    out: *mut *mut c_void,
) -> c_int {
    unsafe {
        *out = ptr::null_mut();
        let Some(map) = methods(f).xShmMap else {
            return SQLITE_IOERR_SHMMAP;
        };
        let rc = map(real(f), page, size, 0, out);
        if rc == SQLITE_OK {
            // A same-process writable connection can make readonly_shm reuse
            // its writable mapping. Fail closed before SQLite can modify it.
            *out = ptr::null_mut();
            SQLITE_IOERR_SHMMAP
        } else {
            rc
        }
    }
}
unsafe extern "C" fn shm_lock(f: *mut sqlite3_file, o: c_int, n: c_int, flags: c_int) -> c_int {
    unsafe {
        methods(f)
            .xShmLock
            .map_or(SQLITE_IOERR_SHMLOCK, |call| call(real(f), o, n, flags))
    }
}
unsafe extern "C" fn barrier(f: *mut sqlite3_file) {
    unsafe {
        if let Some(call) = methods(f).xShmBarrier {
            call(real(f));
        }
    }
}
unsafe extern "C" fn unmap(f: *mut sqlite3_file, _: c_int) -> c_int {
    unsafe {
        methods(f)
            .xShmUnmap
            .map_or(SQLITE_OK, |call| call(real(f), 0))
    }
}
unsafe extern "C" fn delete(_: *mut sqlite3_vfs, _: *const c_char, _: c_int) -> c_int {
    SQLITE_READONLY
}

static METHODS: sqlite3_io_methods = sqlite3_io_methods {
    iVersion: 2,
    xClose: Some(close),
    xRead: Some(read),
    xWrite: Some(write),
    xTruncate: Some(truncate),
    xSync: Some(sync),
    xFileSize: Some(size),
    xLock: Some(lock),
    xUnlock: Some(unlock),
    xCheckReservedLock: Some(reserved),
    xFileControl: Some(control),
    xSectorSize: Some(sector),
    xDeviceCharacteristics: Some(device),
    xShmMap: Some(shm_map),
    xShmLock: Some(shm_lock),
    xShmBarrier: Some(barrier),
    xShmUnmap: Some(unmap),
    xFetch: None,
    xUnfetch: None,
};
