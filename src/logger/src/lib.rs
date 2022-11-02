pub use log::*;

static LOGGER: Logger = Logger;

pub fn init(level: LevelFilter) -> Result<(), SetLoggerError> {
    set_logger(&LOGGER).map(|()| set_max_level(level))
}

#[derive(Copy, Clone)]
struct Logger;

impl Log for Logger {
    fn enabled(&self, _: &Metadata) -> bool {
        unreachable!()
    }
    fn log(&self, record: &Record) {
        eprintln!("[{}] {}", record.level(), record.args());
    }
    fn flush(&self) {
        unreachable!()
    }
}
