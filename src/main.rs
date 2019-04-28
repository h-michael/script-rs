use nix::fcntl::{open, OFlag};
use nix::libc::{winsize, STDERR_FILENO, STDIN_FILENO, STDOUT_FILENO, VMIN, VTIME};
use nix::pty::*;
use nix::sys::select::{select, FdSet};
use nix::sys::stat::Mode;
use nix::sys::termios::*;
use nix::unistd::*;
use nix::Result;
use std::ffi::CString;
use std::os::unix::prelude::*;
use std::path::Path;

fn main() {
    let mut ws = winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let mut tty_origin = tcgetattr(STDIN_FILENO).expect("can not get stdin tty");
    unsafe { ioctl::tiocgwinsz(STDIN_FILENO, &mut ws) }.expect("can not ge stdin window size");

    let mut master_fd = None;
    let mut slave_name = None;

    let fork_result = match pty_fork(&mut master_fd, &mut slave_name, Some(&tty_origin), ws) {
        Ok(result) => result,
        Err(e) => panic!("{:?}", e),
    };

    if fork_result.is_child() {
        match std::env::var("SHELL") {
            Ok(shell) => {
                let shell = CString::new(shell.as_str()).unwrap();
                execv(&shell, &[]).expect("can not exec shell");
            },
            Err(_) => {
                let shell = CString::new("/bin/sh").unwrap();
                execv(&shell, &[]).expect("can not exec shell");
            }
        }
    }

    let master_fd = match master_fd {
        Some(fd) => fd,
        None => panic!("master fd is not found"),
    };

    let script_fd = open(
        Path::new("/tmp/script_tmp"),
        OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_TRUNC,
        Mode::S_IRUSR
            | Mode::S_IWUSR
            | Mode::S_IRGRP
            | Mode::S_IWGRP
            | Mode::S_IROTH
            | Mode::S_IWOTH,
    )
    .expect("script_fd");
    tty_set_row(STDIN_FILENO, Some(&mut tty_origin));
    tty_reset(&mut tty_origin).unwrap();

    loop {
        let mut buf: [u8; 256] = [0; 256];
        let mut in_fds = FdSet::new();
        in_fds.insert(STDIN_FILENO);
        in_fds.insert(master_fd);

        select(Some(master_fd + 1), Some(&mut in_fds), None, None, None).unwrap();

        if in_fds.contains(STDIN_FILENO) {
            if read(STDIN_FILENO, &mut buf).is_err() {
                return;
            }
            write(master_fd, &buf).unwrap();
        }

        if in_fds.contains(master_fd) {
            if read(master_fd, &mut buf).is_err() {
                return;
            }
            write(STDOUT_FILENO, &buf).unwrap();
            write(script_fd, &buf).unwrap();
        }
    }
}

fn pty_master_open() -> Result<(nix::pty::PtyMaster, String)> {
    let master_fd = posix_openpt(OFlag::O_RDWR)?;
    grantpt(&master_fd)?;
    unlockpt(&master_fd)?;

    // Get the name of the slave
    let slave_name = unsafe { ptsname(&master_fd) }?;
    Ok((master_fd, slave_name))
}

fn pty_fork(
    master_fd: &mut Option<RawFd>,
    slave_name: &mut Option<String>,
    slave_termios: Option<&Termios>,
    slave_win_size: winsize,
) -> Result<ForkResult> {
    // Open pty master
    let (mfd, slname) = pty_master_open()?;

    if slave_name.is_some() {
        *slave_name = Some(slname.clone());
    }

    // Fork process
    match fork() {
        Ok(ForkResult::Parent { child }) => {
            *master_fd = Some(mfd.into_raw_fd());
            Ok(ForkResult::Parent { child })
        }
        Ok(ForkResult::Child) => {
            // Set session id to child process
            setsid().unwrap();
            close(mfd.into_raw_fd())?;

            let slave_fd = open(Path::new(&slname), OFlag::O_RDWR, Mode::empty())?;

            // For BSD
            if cfg!(target_os = "openbsd") {
                unsafe { ioctl::tiocsctty(0, &slave_fd) }.unwrap();
            }

            if slave_termios.is_some() {
                tcsetattr(slave_fd, SetArg::TCSANOW, &slave_termios.unwrap())?;
            }

            tcsetattr(STDIN_FILENO, SetArg::TCSAFLUSH, &slave_termios.unwrap())?;
            unsafe { ioctl::tiocswinsz(slave_fd, &slave_win_size) }?;

            dup2(slave_fd, STDIN_FILENO)?;
            dup2(slave_fd, STDOUT_FILENO)?;
            dup2(slave_fd, STDERR_FILENO)?;

            close(slave_fd)?;

            Ok(ForkResult::Child)
        }
        Err(err) => {
            close(mfd.into_raw_fd())?;
            panic!("{:?}", err);
        }
    }
}

fn tty_set_row(fd: i32, prev_termios: Option<&mut Termios>) {
    let mut termios = tcgetattr(fd).unwrap();
    if let Some(prev) = prev_termios {
        *prev = termios.clone()
    };

    termios.local_flags &=
        !(LocalFlags::ICANON | LocalFlags::ISIG | LocalFlags::IEXTEN | LocalFlags::ECHO);
    termios.input_flags &= !(InputFlags::BRKINT
        | InputFlags::ICRNL
        | InputFlags::IGNBRK
        | InputFlags::IGNCR
        | InputFlags::INLCR
        | InputFlags::INPCK
        | InputFlags::ISTRIP
        | InputFlags::IXON
        | InputFlags::PARMRK);
    termios.output_flags &= !OutputFlags::OPOST;
    termios.control_chars[VMIN as usize] = 1;
    termios.control_chars[VTIME as usize] = 0;
    tcsetattr(fd, SetArg::TCSAFLUSH, &termios).unwrap();
}

fn tty_reset(tty_origin: &mut Termios) -> Result<()> {
    tcsetattr(STDIN_FILENO, SetArg::TCSANOW, tty_origin)
}

mod ioctl {
    use nix::libc::{winsize, TIOCGWINSZ, TIOCSWINSZ, TIOCSCTTY};
    use nix::*;
    ioctl_write_ptr_bad!(tiocswinsz, TIOCSWINSZ, winsize);
    ioctl_read_bad!(tiocgwinsz, TIOCGWINSZ, winsize);
    ioctl_write_ptr_bad!(tiocsctty, TIOCSCTTY, i32);
}
