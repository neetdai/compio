use arrayvec::ArrayVec;
use compio::{
    buf::IntoInner,
    driver::{AsRawFd, Driver, Entry, Poller},
};

fn main() {
    let mut driver = Driver::new().unwrap();
    let file = compio::fs::File::open("Cargo.toml").unwrap();
    driver.attach(file.as_raw_fd()).unwrap();

    let mut op = compio::op::ReadAt::new(file.as_raw_fd(), 0, Vec::with_capacity(4096));
    unsafe { driver.push(&mut op, 0) }.unwrap();

    let mut entries = ArrayVec::<Entry, 1>::new();
    driver.poll(None, &mut entries).unwrap();
    let entry = entries.drain(..).next().unwrap();
    assert_eq!(entry.user_data(), 0);

    let n = entry.into_result().unwrap();
    let mut buffer = op.into_inner().into_inner();
    unsafe {
        buffer.set_len(n);
    }
    println!("{}", String::from_utf8(buffer).unwrap());
}
