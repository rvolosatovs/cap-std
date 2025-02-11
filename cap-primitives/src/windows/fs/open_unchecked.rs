use super::get_path::concatenate;
use super::open_options_to_std;
use crate::fs::{errors, FollowSymlinks, OpenOptions, OpenUncheckedError, SymlinkKind};
use crate::{ambient_authority, AmbientAuthority};
use std::os::windows::fs::MetadataExt;
use std::path::Path;
use std::{fs, io};
use windows_sys::Win32::Foundation;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_DIRECTORY, FILE_FLAG_OPEN_REPARSE_POINT,
};

/// *Unsandboxed* function similar to `open`, but which does not perform
/// sandboxing.
pub(crate) fn open_unchecked(
    start: &fs::File,
    path: &Path,
    options: &OpenOptions,
) -> Result<fs::File, OpenUncheckedError> {
    let full_path = concatenate(start, path).map_err(OpenUncheckedError::Other)?;
    open_ambient_impl(&full_path, options, ambient_authority())
}

/// *Unsandboxed* function similar to `open_unchecked`, but which just operates
/// on a bare path, rather than starting with a handle.
pub(crate) fn open_ambient_impl(
    path: &Path,
    options: &OpenOptions,
    ambient_authority: AmbientAuthority,
) -> Result<fs::File, OpenUncheckedError> {
    let _ = ambient_authority;
    let (opts, manually_trunc) = open_options_to_std(options);
    match opts.open(path) {
        Ok(f) => {
            let enforce_dir = options.dir_required;
            let enforce_nofollow = options.follow == FollowSymlinks::No
                && (options.ext.custom_flags & FILE_FLAG_OPEN_REPARSE_POINT) == 0;

            if enforce_dir || enforce_nofollow {
                let metadata = f.metadata().map_err(OpenUncheckedError::Other)?;

                if enforce_dir {
                    // Require a directory. It may seem possible to eliminate
                    // this `metadata()` call by appending a slash to the path
                    // before opening it so that the OS requires a directory
                    // for us, however on Windows in some circumstances this
                    // leads to "The filename, directory name, or volume label
                    // syntax is incorrect." errors.
                    //
                    // We check `file_attributes()` instead of using `is_dir()`
                    // since the latter returns false if we're looking at a
                    // directory symlink.
                    if metadata.file_attributes() & FILE_ATTRIBUTE_DIRECTORY == 0 {
                        return Err(OpenUncheckedError::Other(errors::is_not_directory()));
                    }
                }

                if enforce_nofollow {
                    // Windows doesn't have a way to return errors like
                    // `O_NOFOLLOW`, so if we're not following symlinks and
                    // we're not using `FILE_FLAG_OPEN_REPARSE_POINT` manually
                    // to open a symlink itself, check for symlinks and report
                    // them as a distinct error.
                    if metadata.file_type().is_symlink() {
                        return Err(OpenUncheckedError::Symlink(
                            io::Error::from_raw_os_error(
                                Foundation::ERROR_STOPPED_ON_SYMLINK as i32,
                            ),
                            if metadata.file_attributes() & FILE_ATTRIBUTE_DIRECTORY
                                == FILE_ATTRIBUTE_DIRECTORY
                            {
                                SymlinkKind::Dir
                            } else {
                                SymlinkKind::File
                            },
                        ));
                    }
                }
            }

            // Windows truncates symlinks into normal files, so truncation
            // may be disabled above; do it manually if needed.
            if manually_trunc {
                // Unwrap is ok because 0 never overflows, and we'll only
                // have `manually_trunc` set when the file is opened for
                // writing.
                f.set_len(0).unwrap();
            }
            Ok(f)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Err(OpenUncheckedError::NotFound(e)),
        Err(e) => match e.raw_os_error() {
            Some(code) => match code as u32 {
                Foundation::ERROR_FILE_NOT_FOUND | Foundation::ERROR_PATH_NOT_FOUND => {
                    Err(OpenUncheckedError::NotFound(e))
                }
                _ => Err(OpenUncheckedError::Other(e)),
            },
            None => Err(OpenUncheckedError::Other(e)),
        },
    }
}
