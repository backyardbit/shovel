use anyhow::{bail, Context, Result};
use des::cipher::{generic_array::GenericArray, BlockEncrypt, KeyInit};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::thread::sleep;
use std::time::Duration;

pub const BUTTON_LEFT: u8 = 1;
pub const BUTTON_MIDDLE: u8 = 2;
pub const BUTTON_RIGHT: u8 = 4;
const WHEEL_UP: u8 = 8;
const WHEEL_DOWN: u8 = 16;
const WHEEL_LEFT: u8 = 32;
const WHEEL_RIGHT: u8 = 64;

pub struct Vnc {
    stream: TcpStream,
    pub width: u16,
    pub height: u16,
}

impl Vnc {
    pub fn connect(port: u16, password: &str) -> Result<Self> {
        if port == 0 {
            bail!("the VM has no VNC endpoint (the host could not start the VZ VNC server)");
        }
        let stream = TcpStream::connect(("127.0.0.1", port))
            .or_else(|_| TcpStream::connect(("::1", port)))
            .with_context(|| format!("cannot reach VNC server on port {port}"))?;
        stream.set_read_timeout(Some(Duration::from_secs(20)))?;
        stream.set_nodelay(true)?;
        let mut vnc = Vnc { stream, width: 0, height: 0 };
        vnc.handshake(password)?;
        Ok(vnc)
    }

    fn handshake(&mut self, password: &str) -> Result<()> {
        let mut version = [0u8; 12];
        self.stream.read_exact(&mut version)?;
        let text = String::from_utf8_lossy(&version);
        let minor: u32 = text.get(8..11).and_then(|m| m.parse().ok()).unwrap_or(3);
        let minor = if minor >= 8 { 8 } else if minor == 7 { 7 } else { 3 };
        self.stream.write_all(format!("RFB 003.{minor:03}\n").as_bytes())?;

        let security = if minor == 3 {
            self.read_u32()? as u8
        } else {
            let count = self.read_u8()?;
            if count == 0 {
                bail!("VNC server refused connection: {}", self.read_reason()?);
            }
            let mut types = vec![0u8; count as usize];
            self.stream.read_exact(&mut types)?;
            let chosen = if types.contains(&2) {
                2
            } else if types.contains(&1) {
                1
            } else {
                bail!("VNC server offers no supported security types: {types:?}");
            };
            self.stream.write_all(&[chosen])?;
            chosen
        };

        if security == 2 {
            let mut challenge = [0u8; 16];
            self.stream.read_exact(&mut challenge)?;
            let response = des_response(password, &challenge);
            self.stream.write_all(&response)?;
        }
        if security == 2 || minor == 8 {
            if self.read_u32()? != 0 {
                let reason = if minor == 8 { self.read_reason().unwrap_or_default() } else { String::new() };
                bail!("VNC authentication failed {reason}");
            }
        }

        self.stream.write_all(&[1])?;
        self.width = self.read_u16()?;
        self.height = self.read_u16()?;
        let mut pixel_format = [0u8; 16];
        self.stream.read_exact(&mut pixel_format)?;
        let name_len = self.read_u32()? as usize;
        let mut name = vec![0u8; name_len];
        self.stream.read_exact(&mut name)?;

        let mut set_format = vec![0u8, 0, 0, 0];
        set_format.extend_from_slice(&[32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]);
        self.stream.write_all(&set_format)?;

        let encodings: [i32; 2] = [0, -223];
        let mut set_encodings = vec![2u8, 0];
        set_encodings.extend_from_slice(&(encodings.len() as u16).to_be_bytes());
        for e in encodings {
            set_encodings.extend_from_slice(&e.to_be_bytes());
        }
        self.stream.write_all(&set_encodings)?;
        Ok(())
    }

    fn read_u8(&mut self) -> Result<u8> {
        let mut b = [0u8; 1];
        self.stream.read_exact(&mut b)?;
        Ok(b[0])
    }

    fn read_u16(&mut self) -> Result<u16> {
        let mut b = [0u8; 2];
        self.stream.read_exact(&mut b)?;
        Ok(u16::from_be_bytes(b))
    }

    fn read_u32(&mut self) -> Result<u32> {
        let mut b = [0u8; 4];
        self.stream.read_exact(&mut b)?;
        Ok(u32::from_be_bytes(b))
    }

    fn read_reason(&mut self) -> Result<String> {
        let len = self.read_u32()? as usize;
        let mut reason = vec![0u8; len.min(4096)];
        self.stream.read_exact(&mut reason)?;
        Ok(String::from_utf8_lossy(&reason).into_owned())
    }

    fn skip(&mut self, n: usize) -> Result<()> {
        let mut buf = vec![0u8; n];
        self.stream.read_exact(&mut buf)?;
        Ok(())
    }

    fn request_full(&mut self) -> Result<()> {
        let mut msg = vec![3u8, 0, 0, 0, 0, 0];
        msg.extend_from_slice(&self.width.to_be_bytes());
        msg.extend_from_slice(&self.height.to_be_bytes());
        self.stream.write_all(&msg)?;
        Ok(())
    }

    pub fn capture(&mut self) -> Result<(u32, u32, Vec<u8>)> {
        let mut rgb = vec![0u8; self.width as usize * self.height as usize * 3];
        let mut covered = 0usize;
        self.request_full()?;
        loop {
            match self.read_u8()? {
                0 => {
                    self.skip(1)?;
                    let rects = self.read_u16()?;
                    for _ in 0..rects {
                        let x = self.read_u16()? as usize;
                        let y = self.read_u16()? as usize;
                        let w = self.read_u16()? as usize;
                        let h = self.read_u16()? as usize;
                        let encoding = self.read_u32()? as i32;
                        match encoding {
                            0 => {
                                let mut raw = vec![0u8; w * h * 4];
                                self.stream.read_exact(&mut raw)?;
                                let fw = self.width as usize;
                                let fh = self.height as usize;
                                for row in 0..h {
                                    let ty = y + row;
                                    if ty >= fh {
                                        break;
                                    }
                                    for col in 0..w {
                                        let tx = x + col;
                                        if tx >= fw {
                                            break;
                                        }
                                        let src = (row * w + col) * 4;
                                        let dst = (ty * fw + tx) * 3;
                                        rgb[dst] = raw[src + 2];
                                        rgb[dst + 1] = raw[src + 1];
                                        rgb[dst + 2] = raw[src];
                                    }
                                }
                                covered += w * h;
                            }
                            -223 => {
                                self.width = w as u16;
                                self.height = h as u16;
                                rgb = vec![0u8; w * h * 3];
                                covered = 0;
                                self.request_full()?;
                            }
                            other => bail!("VNC server sent unsupported encoding {other}"),
                        }
                    }
                    if covered >= self.width as usize * self.height as usize {
                        return Ok((self.width as u32, self.height as u32, rgb));
                    }
                }
                1 => {
                    self.skip(3)?;
                    let n = self.read_u16()? as usize;
                    self.skip(n * 6)?;
                }
                2 => {}
                3 => {
                    self.skip(3)?;
                    let n = self.read_u32()? as usize;
                    self.skip(n)?;
                }
                other => bail!("unexpected VNC message type {other}"),
            }
        }
    }

    pub fn screenshot(&mut self, path: &Path) -> Result<(u32, u32)> {
        let (w, h, rgb) = self.capture()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::File::create(path)?;
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), w, h);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        encoder.write_header()?.write_image_data(&rgb)?;
        Ok((w, h))
    }

    pub fn pointer(&mut self, mask: u8, x: u16, y: u16) -> Result<()> {
        let mut msg = vec![5u8, mask];
        msg.extend_from_slice(&x.to_be_bytes());
        msg.extend_from_slice(&y.to_be_bytes());
        self.stream.write_all(&msg)?;
        Ok(())
    }

    pub fn key(&mut self, down: bool, keysym: u32) -> Result<()> {
        let mut msg = vec![4u8, down as u8, 0, 0];
        msg.extend_from_slice(&keysym.to_be_bytes());
        self.stream.write_all(&msg)?;
        Ok(())
    }

    pub fn click(&mut self, x: u16, y: u16, button: u8, count: u32) -> Result<()> {
        self.pointer(0, x, y)?;
        pause(40);
        for _ in 0..count {
            self.pointer(button, x, y)?;
            pause(40);
            self.pointer(0, x, y)?;
            pause(60);
        }
        Ok(())
    }

    pub fn drag(&mut self, from: (u16, u16), to: (u16, u16)) -> Result<()> {
        self.pointer(0, from.0, from.1)?;
        pause(40);
        self.pointer(BUTTON_LEFT, from.0, from.1)?;
        pause(80);
        let steps = 20;
        for i in 1..=steps {
            let x = from.0 as i32 + (to.0 as i32 - from.0 as i32) * i / steps;
            let y = from.1 as i32 + (to.1 as i32 - from.1 as i32) * i / steps;
            self.pointer(BUTTON_LEFT, x as u16, y as u16)?;
            pause(15);
        }
        pause(80);
        self.pointer(0, to.0, to.1)?;
        Ok(())
    }

    pub fn scroll(&mut self, x: u16, y: u16, dx: i32, dy: i32) -> Result<()> {
        self.pointer(0, x, y)?;
        pause(30);
        let vertical = if dy > 0 { WHEEL_DOWN } else { WHEEL_UP };
        for _ in 0..dy.unsigned_abs() {
            self.pointer(vertical, x, y)?;
            self.pointer(0, x, y)?;
            pause(15);
        }
        let horizontal = if dx > 0 { WHEEL_RIGHT } else { WHEEL_LEFT };
        for _ in 0..dx.unsigned_abs() {
            self.pointer(horizontal, x, y)?;
            self.pointer(0, x, y)?;
            pause(15);
        }
        Ok(())
    }

    pub fn finish(mut self) -> Result<()> {
        self.stream.flush()?;
        pause(150);
        Ok(())
    }
}

pub fn pause(ms: u64) {
    sleep(Duration::from_millis(ms));
}

fn des_response(password: &str, challenge: &[u8; 16]) -> [u8; 16] {
    let mut key = [0u8; 8];
    for (i, b) in password.bytes().take(8).enumerate() {
        key[i] = b.reverse_bits();
    }
    let cipher = des::Des::new_from_slice(&key).expect("8-byte DES key");
    let mut out = [0u8; 16];
    for half in 0..2 {
        let mut block = GenericArray::clone_from_slice(&challenge[half * 8..half * 8 + 8]);
        cipher.encrypt_block(&mut block);
        out[half * 8..half * 8 + 8].copy_from_slice(&block);
    }
    out
}
