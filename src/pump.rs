use esp_idf_hal::gpio::{Output, OutputPin, PinDriver};
use log::{info, warn};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PumpType {
    NutrientA,
    NutrientB,
    PhUp,
    PhDown,
}

pub struct PumpController<A, B, C, D>
where
    A: OutputPin,
    B: OutputPin,
    C: OutputPin,
    D: OutputPin,
{
    pump_a: PinDriver<'static, A, Output>,
    pump_b: PinDriver<'static, B, Output>,
    pump_ph_up: PinDriver<'static, C, Output>,
    pump_ph_down: PinDriver<'static, D, Output>,
}

impl<A, B, C, D> PumpController<A, B, C, D>
where
    A: OutputPin,
    B: OutputPin,
    C: OutputPin,
    D: OutputPin,
{
    pub fn new(
        mut pump_a: PinDriver<'static, A, Output>,
        mut pump_b: PinDriver<'static, B, Output>,
        mut pump_ph_up: PinDriver<'static, C, Output>,
        mut pump_ph_down: PinDriver<'static, D, Output>,
    ) -> anyhow::Result<Self> {
        pump_a.set_low()?;
        pump_b.set_low()?;
        pump_ph_up.set_low()?;
        pump_ph_down.set_low()?;

        info!("Đã khởi tạo PumpController và tắt toàn bộ bơm.");

        Ok(Self {
            pump_a,
            pump_b,
            pump_ph_up,
            pump_ph_down,
        })
    }

    pub fn set_pump_state(&mut self, pump: PumpType, state: bool) -> anyhow::Result<()> {
        match pump {
            PumpType::NutrientA => {
                if state {
                    self.pump_a.set_high()?;
                } else {
                    self.pump_a.set_low()?;
                }
            }
            PumpType::NutrientB => {
                if state {
                    self.pump_b.set_high()?;
                } else {
                    self.pump_b.set_low()?;
                }
            }
            PumpType::PhUp => {
                if state {
                    self.pump_ph_up.set_high()?;
                } else {
                    self.pump_ph_up.set_low()?;
                }
            }
            PumpType::PhDown => {
                if state {
                    self.pump_ph_down.set_high()?;
                } else {
                    self.pump_ph_down.set_low()?;
                }
            }
        }

        if state {
            info!("Bật bơm {:?}", pump);
        } else {
            info!("Tắt bơm {:?}", pump);
        }

        Ok(())
    }

    pub fn pulse_pump(&mut self, pump: PumpType, duration_ms: u64) -> anyhow::Result<()> {
        self.set_pump_state(pump, true)?;

        std::thread::sleep(std::time::Duration::from_millis(duration_ms));

        self.set_pump_state(pump, false)?;
        Ok(())
    }

    pub fn stop_all(&mut self) -> anyhow::Result<()> {
        warn!("CẢNH BÁO: Kích hoạt dừng khẩn cấp (Emergency Shutdown). Tắt toàn bộ bơm!");
        self.pump_a.set_low()?;
        self.pump_b.set_low()?;
        self.pump_ph_up.set_low()?;
        self.pump_ph_down.set_low()?;
        Ok(())
    }
}
