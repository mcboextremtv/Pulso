use pulso_core::player::{PlayState, Player};
use std::{time::Duration, path::PathBuf};
fn main() {
    let p = Player::new();
    p.play(PathBuf::from(std::env::args().nth(1).unwrap()));
    std::thread::sleep(Duration::from_millis(150));
    p.pause();
    std::thread::sleep(Duration::from_millis(200));
    let before = p.status();
    println!("en pausa: {:?} pos {:?}", before.state, before.position);
    p.seek(Duration::from_secs(10));
    std::thread::sleep(Duration::from_millis(500));
    let mid = p.status();
    println!("tras seek en pausa: {:?} pos {:?}", mid.state, mid.position);
    p.set_volume(0.5); // si el hilo estuviera bloqueado, esto no tendría efecto visible
    p.resume();
    std::thread::sleep(Duration::from_millis(300));
    let after = p.status();
    println!("tras reanudar: {:?} pos {:?}", after.state, after.position);
    println!("{}", if after.state == PlayState::Playing || after.finished { "OK: el hilo de audio responde" } else { "FALLO" });
}
