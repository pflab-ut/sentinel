pub fn split_last(path: &str) -> (&str, &str) {
    match path {
        "" => (".", "."),
        "/" => ("/", "."),
        _ => match path.rsplit_once('/') {
            Some((dir, file)) => {
                if dir.is_empty() {
                    ("/", &file[1..])
                } else {
                    (dir, file)
                }
            }
            None => (".", path),
        },
    }
}

pub fn io_err_from_nix_errno() -> std::io::Error {
    std::io::Error::from_raw_os_error(nix::errno::errno())
}
