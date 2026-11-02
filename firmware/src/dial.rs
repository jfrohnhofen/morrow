//! Rotary encoder decoding.
//!
//! The transition tables are carried over unchanged from `dial.py`. They decode
//! a full quadrature cycle before emitting anything, which is what debounces a
//! detented encoder without any timing involved: electrical noise walks the
//! machine back to [`DialState::Start`] instead of producing a count.

use embassy_rp::Peri;
use embassy_rp::gpio::{Input, Level, Pin, Pull};
use embassy_sync::channel::Sender;

/// Something the user did with the dial.
#[derive(Copy, Clone, PartialEq, Eq, defmt::Format)]
pub enum Event {
    /// Rotated anticlockwise. Named for the original firmware's `"LEFT"`.
    Left,
    /// Rotated clockwise. Named for the original firmware's `"RIGHT"`.
    Right,
    /// Button went down. The UI only tracks this as activity.
    Press,
    /// Button came back up. This is the event the UI acts on, so a click takes
    /// effect on release rather than on contact.
    Release,
}

/// Quadrature decoder states. A full detent is three steps in one direction.
#[derive(Copy, Clone)]
enum DialState {
    Start = 0,
    Cw1 = 1,
    Cw2 = 2,
    Cw3 = 3,
    Ccw1 = 4,
    Ccw2 = 5,
    Ccw3 = 6,
}

/// Next state and emitted event for each (state, CLK/DT) pair.
///
/// Columns are the two-bit `CLK << 1 | DT` reading: 00, 01, 10, 11.
const DIAL_TRANSITIONS: [[(DialState, Option<Event>); 4]; 7] = {
    use DialState::*;
    [
        // Start
        [(Start, None), (Ccw1, None), (Cw1, None), (Start, None)],
        // Cw1
        [(Cw2, None), (Start, None), (Cw1, None), (Start, None)],
        // Cw2
        [(Cw2, None), (Cw3, None), (Cw1, None), (Start, None)],
        // Cw3 — a complete clockwise detent lands back on 11.
        [(Cw2, None), (Cw3, None), (Start, None), (Start, Some(Event::Left))],
        // Ccw1
        [(Ccw2, None), (Ccw1, None), (Start, None), (Start, None)],
        // Ccw2
        [(Ccw2, None), (Ccw1, None), (Ccw3, None), (Start, None)],
        // Ccw3 — a complete anticlockwise detent lands back on 11.
        [(Ccw2, None), (Start, None), (Ccw3, None), (Start, Some(Event::Right))],
    ]
};

/// Button states. The switch is active low with a pull-up, so index 0 is pressed.
#[derive(Copy, Clone)]
enum BtnState {
    Up = 0,
    Down = 1,
}

/// Next state and emitted event for each (state, pin level) pair.
const BTN_TRANSITIONS: [[(BtnState, Option<Event>); 2]; 2] = {
    use BtnState::*;
    [
        // Up: a low reading is a press.
        [(Down, Some(Event::Press)), (Up, None)],
        // Down: a high reading is a release.
        [(Down, None), (Up, Some(Event::Release))],
    ]
};

/// Watches the three encoder pins and reports what the user did.
pub struct Dial {
    btn: Input<'static>,
    clk: Input<'static>,
    dt: Input<'static>,
    dial_state: DialState,
    btn_state: BtnState,
}

impl Dial {
    /// Claims the encoder pins. All three are pulled up, as the module expects.
    pub fn new(
        btn: Peri<'static, impl Pin>,
        clk: Peri<'static, impl Pin>,
        dt: Peri<'static, impl Pin>,
    ) -> Dial {
        Dial {
            btn: Input::new(btn, Pull::Up),
            clk: Input::new(clk, Pull::Up),
            dt: Input::new(dt, Pull::Up),
            dial_state: DialState::Start,
            btn_state: BtnState::Up,
        }
    }

    /// Waits for an edge on any pin, then runs both state machines.
    ///
    /// Both are evaluated on every edge regardless of which pin moved, exactly
    /// as the single shared interrupt handler in `dial.py` did. Either machine
    /// may emit, so this can send two events for one edge.
    pub async fn run<const N: usize>(
        &mut self,
        tx: Sender<'static, embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex, Event, N>,
    ) -> ! {
        loop {
            embassy_futures::select::select3(
                self.btn.wait_for_any_edge(),
                self.clk.wait_for_any_edge(),
                self.dt.wait_for_any_edge(),
            )
            .await;

            let clk_dt = ((self.clk.get_level() == Level::High) as usize) << 1
                | (self.dt.get_level() == Level::High) as usize;
            let (next, evt) = DIAL_TRANSITIONS[self.dial_state as usize][clk_dt];
            self.dial_state = next;
            if let Some(e) = evt {
                defmt::info!("dial: {}", e);
                // Dropping an event is better than stalling the input task; the
                // queue only fills if the UI is already far behind.
                let _ = tx.try_send(e);
            }

            let btn = (self.btn.get_level() == Level::High) as usize;
            let (next, evt) = BTN_TRANSITIONS[self.btn_state as usize][btn];
            self.btn_state = next;
            if let Some(e) = evt {
                defmt::info!("dial: {}", e);
                let _ = tx.try_send(e);
            }
        }
    }
}
