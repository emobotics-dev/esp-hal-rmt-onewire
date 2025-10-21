#![no_std]
#![no_main]

use core::cell::OnceCell;
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use esp_hal::{clock::CpuClock, peripherals::GPIO26, rmt::*, system::Stack, time::Rate, Async};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex;

use esp_hal::interrupt::software::SoftwareInterruptControl;
use esp_hal::timer::timg::TimerGroup;
use esp_hal_rmt_onewire::*;
use esp_println::println;
use esp_rtos::embassy::Executor;
use static_cell::StaticCell;
use esp_backtrace as _;
use esp_hal::peripherals::RMT;

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_rtos::main]
async fn main(_spawner: Spawner) -> ! {
    static APP_CORE_STACK: StaticCell<Stack<8192>> = StaticCell::new();
    let app_core_stack = APP_CORE_STACK.init(Stack::new());

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(
        timg0.timer0,
        #[cfg(target_arch = "riscv32")]
        sw_int.software_interrupt0,
    );

    esp_rtos::start_second_core(
        peripherals.CPU_CTRL,
        #[cfg(target_arch = "xtensa")]
        sw_int.software_interrupt0,
        sw_int.software_interrupt1,
        app_core_stack,
        move || {
            static EXECUTOR: StaticCell<Executor> = StaticCell::new();
            let executor = EXECUTOR.init(Executor::new());
            executor.run(|spawner| {
                spawner.must_spawn(ow_task(peripherals.RMT, peripherals.GPIO26));
            });
        },
    );
    loop {}
}

#[embassy_executor::task]
async fn ow_task(_rmt: RMT<'static>, gpio: GPIO26<'static>) -> ! {
    let rmt = Rmt::new(_rmt, Rate::from_mhz(80_u32))
        .unwrap()
        .into_async();


    #[cfg(target_arch = "riscv32")]
    let gpio = peripherals.GPIO6;
    #[cfg(not(target_arch = "riscv32"))]
    let mut ow = OneWire::new(rmt.channel0, rmt.channel2, gpio).unwrap();

    loop {
        println!("Resetting the bus");
        ow.reset().await.unwrap();

        println!("Broadcasting a measure temperature command to all attached sensors");
        for a in [0xCC, 0x44] {
            ow.send_byte(a).await.unwrap();
        }

        println!("Scanning the bus to retrieve the measured temperatures");
        search(&mut ow).await;

        println!("Waiting for 10 seconds");
        Timer::after(Duration::from_secs(10)).await;

    }
}

// Temperature in C
#[derive(Ord, PartialOrd, PartialEq, Eq, Debug)]
pub struct Temperature(pub fixed::types::I12F4);

const CTOF_FACT: fixed::types::I12F4 = fixed::types::I12F4::lit("1.8");
const CTOF_OFF: fixed::types::I12F4 = fixed::types::I12F4::lit("32");

impl core::fmt::Display for Temperature {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> Result<(), core::fmt::Error> {
        write!(f, "{}°F ({}°C)", self.0 * CTOF_FACT + CTOF_OFF, self.0)?;
        Ok(())
    }
}

pub async fn search<'a>(ow: &mut OneWire<'a>) -> () {
    let mut search = Search::new();
    loop {
        match search.next(ow).await {
            Ok(address) => {
                println!("Reading device {:?}", address);
                ow.reset().await.unwrap();
                ow.send_byte(0x55).await.unwrap();
                ow.send_address(address).await.unwrap();
                ow.send_byte(0xBE).await.unwrap();
                let temp_low = ow
                    .exchange_byte(0xFF)
                    .await
                    .expect("failed to get low byte of temperature");
                let temp_high = ow
                    .exchange_byte(0xFF)
                    .await
                    .expect("failed to get high byte of temperature");
                let temp = fixed::types::I12F4::from_le_bytes([temp_low, temp_high]);
                println!("Temp is: {temp}");
            }
            Err(_) => {
                println!("End of search");
                return ();
            }
        }
    }
}
