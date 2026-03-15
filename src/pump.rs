use esp_idf_hal::gpio::{Output, OutputPin, PinDriver};
use log::{info, warn};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PumpType {
    NutrientA,
    NutrientB,
    PhUp,
    PhDown,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WaterDirection {
    In,   // Cấp nước
    Out,  // Xả nước
    Stop, // Dừng
}

// Thêm 2 Generic PG, PH cho IN3 và IN4
pub struct PumpController<PA, PB, PC, PD, PE, PF, PG, PH>
where
    PA: OutputPin,
    PB: OutputPin,
    PC: OutputPin,
    PD: OutputPin,
    PE: OutputPin,
    PF: OutputPin,
    PG: OutputPin,
    PH: OutputPin,
{
    pump_a: PinDriver<'static, PA, Output>,
    pump_b: PinDriver<'static, PB, Output>,
    pump_ph_up: PinDriver<'static, PC, Output>,
    pump_ph_down: PinDriver<'static, PD, Output>,

    // Động cơ A (Bơm đảo chiều Cấp/Xả)
    l298n_in1: PinDriver<'static, PE, Output>,
    l298n_in2: PinDriver<'static, PF, Output>,

    // Động cơ B (Bơm nước lên buồng trồng)
    l298n_in3: PinDriver<'static, PG, Output>,
    l298n_in4: PinDriver<'static, PH, Output>,
}

impl<PA, PB, PC, PD, PE, PF, PG, PH> PumpController<PA, PB, PC, PD, PE, PF, PG, PH>
where
    PA: OutputPin,
    PB: OutputPin,
    PC: OutputPin,
    PD: OutputPin,
    PE: OutputPin,
    PF: OutputPin,
    PG: OutputPin,
    PH: OutputPin,
{
    pub fn new(
        mut pump_a: PinDriver<'static, PA, Output>,
        mut pump_b: PinDriver<'static, PB, Output>,
        mut pump_ph_up: PinDriver<'static, PC, Output>,
        mut pump_ph_down: PinDriver<'static, PD, Output>,
        mut l298n_in1: PinDriver<'static, PE, Output>,
        mut l298n_in2: PinDriver<'static, PF, Output>,
        mut l298n_in3: PinDriver<'static, PG, Output>,
        mut l298n_in4: PinDriver<'static, PH, Output>,
    ) -> anyhow::Result<Self> {
        // Tắt toàn bộ khi khởi động
        pump_a.set_low()?;
        pump_b.set_low()?;
        pump_ph_up.set_low()?;
        pump_ph_down.set_low()?;

        l298n_in1.set_low()?;
        l298n_in2.set_low()?;
        l298n_in3.set_low()?;
        l298n_in4.set_low()?;

        info!("Đã khởi tạo PumpController (Quản lý Full 2 kênh L298N).");

        Ok(Self {
            pump_a,
            pump_b,
            pump_ph_up,
            pump_ph_down,
            l298n_in1,
            l298n_in2,
            l298n_in3,
            l298n_in4,
        })
    }

    // (Giữ nguyên hàm set_pump_state và set_water_pump như cũ...)
    pub fn set_pump_state(&mut self, pump: PumpType, state: bool) -> anyhow::Result<()> {
        match pump {
            PumpType::NutrientA => {
                if state {
                    self.pump_a.set_high()?
                } else {
                    self.pump_a.set_low()?
                }
            }
            PumpType::NutrientB => {
                if state {
                    self.pump_b.set_high()?
                } else {
                    self.pump_b.set_low()?
                }
            }
            PumpType::PhUp => {
                if state {
                    self.pump_ph_up.set_high()?
                } else {
                    self.pump_ph_up.set_low()?
                }
            }
            PumpType::PhDown => {
                if state {
                    self.pump_ph_down.set_high()?
                } else {
                    self.pump_ph_down.set_low()?
                }
            }
        }
        Ok(())
    }

    pub fn set_water_pump(&mut self, direction: WaterDirection) -> anyhow::Result<()> {
        self.l298n_in1.set_low()?;
        self.l298n_in2.set_low()?;
        match direction {
            WaterDirection::In => {
                std::thread::sleep(std::time::Duration::from_millis(100));
                self.l298n_in1.set_high()?;
            }
            WaterDirection::Out => {
                std::thread::sleep(std::time::Duration::from_millis(100));
                self.l298n_in2.set_high()?;
            }
            WaterDirection::Stop => {}
        }
        Ok(())
    }

    // --- THÊM MỚI: ĐIỀU KHIỂN BƠM BUỒNG TRỒNG (CHAMBER PUMP) ---
    pub fn set_chamber_pump(&mut self, state: bool) -> anyhow::Result<()> {
        if state {
            self.l298n_in4.set_low()?; // Đảm bảo IN4 tắt trước
            self.l298n_in3.set_high()?; // Bật IN3 để chạy 1 chiều
                                        // (Nếu motor quay ngược chiều bạn muốn, đổi HIGH/LOW giữa IN3 và IN4)
        } else {
            self.l298n_in3.set_low()?;
            self.l298n_in4.set_low()?;
        }
        Ok(())
    }

    // --- CẬP NHẬT DỪNG KHẨN CẤP ---
    pub fn stop_all(&mut self) -> anyhow::Result<()> {
        warn!("CẢNH BÁO: Kích hoạt ngắt khẩn cấp!");
        self.pump_a.set_low()?;
        self.pump_b.set_low()?;
        self.pump_ph_up.set_low()?;
        self.pump_ph_down.set_low()?;

        self.l298n_in1.set_low()?;
        self.l298n_in2.set_low()?;
        self.l298n_in3.set_low()?;
        self.l298n_in4.set_low()?; // Tắt thêm bơm buồng trồng
        Ok(())
    }
}
