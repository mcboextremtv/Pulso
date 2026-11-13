//! Prueba del motor sin interfaz: arranque, posición, salto, fin de pista, cortes.
use pulso_core::player::{PlayState, Player};
use std::{path::PathBuf, time::{Duration, Instant}};

fn main() {
    let path = PathBuf::from(std::env::args().nth(1).expect("uso: engine_test <archivo>"));
    let p = Player::new();
    let t0 = Instant::now();
    p.play(&path);
    let mut started = None;
    let mut sought = false;
    loop {
        std::thread::sleep(Duration::from_millis(100));
        let s = p.status();
        if let Some(e) = &s.error { println!("ERROR: {e}"); return; }
        if started.is_none() && s.state == PlayState::Playing {
            started = Some(t0.elapsed());
            println!("suena tras {:?}", started.unwrap());
        }
        if !sought && s.position > Duration::from_secs(1) {
            sought = true;
            println!("pos antes del salto: {:?}", s.position);
            p.seek(Duration::from_secs(20));
            std::thread::sleep(Duration::from_millis(300));
            println!("pos tras saltar a 20 s: {:?}", p.status().position);
        }
        if p.take_finished() {
            println!("fin de pista detectado a los {:?}; cortes: {}", t0.elapsed(), p.status().underruns);
            return;
        }
        if t0.elapsed() > Duration::from_secs(60) { println!("TIMEOUT"); return; }
    }
}
