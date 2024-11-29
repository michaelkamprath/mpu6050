//! # Mpu6050 sensor driver.
//!
//! `embedded_hal` based driver with i2c access to MPU6050
//!
//! ### Misc
//! * [Register sheet](https://www.invensense.com/wp-content/uploads/2015/02/MPU-6000-Register-Map1.pdf),
//! * [Data sheet](https://www.invensense.com/wp-content/uploads/2015/02/MPU-6500-Datasheet2.pdf)
//!
//! To use this driver you must provide a concrete `embedded_hal` implementation.
//! This example uses `linux_embedded_hal`.
//!
//! **More Examples** can be found [here](https://github.com/juliangaal/mpu6050/tree/master/examples).
//! ```no_run
//! use mpu6050::*;
//! use linux_embedded_hal::{I2cdev, Delay};
//! use i2cdev::linux::LinuxI2CError;
//!
//! fn main() -> Result<(), Mpu6050Error<LinuxI2CError>> {
//!     let i2c = I2cdev::new("/dev/i2c-1")
//!         .map_err(Mpu6050Error::I2c)?;
//!
//!     let mut delay = Delay;
//!     let mut mpu = Mpu6050::new(i2c);
//!
//!     mpu.init(&mut delay)?;
//!
//!     loop {
//!         // get roll and pitch estimate
//!         let acc = mpu.get_acc_angles()?;
//!         println!("r/p: {:?}", acc);
//!
//!         // get sensor temp
//!         let temp = mpu.get_temp()?;
//!         printlnasd!("temp: {:?}c", temp);
//!
//!         // get gyro data, scaled with sensitivity
//!         let gyro = mpu.get_gyro()?;
//!         println!("gyro: {:?}", gyro);
//!
//!         // get accelerometer data, scaled with sensitivity
//!         let acc = mpu.get_acc()?;
//!         println!("acc: {:?}", acc);
//!     }
//! }
//! ```

#![no_std]

mod bits;
pub mod device;

extern crate alloc;

use crate::device::*;
use embedded_hal::{
    delay::DelayNs,
    i2c::I2c,
};
#[allow(unused_imports)]
use micromath::{
    vector::{Vector2d, Vector3d},
    F32Ext,
};
#[cfg(feature = "defmt")]
use defmt::{Format, info, debug};

/// PI, f32
pub const PI: f32 = core::f32::consts::PI;

/// PI / 180, for conversion to radians
pub const PI_180: f32 = PI / 180.0;

/// All possible errors in this crate
#[derive(Debug)]
pub enum Mpu6050Error<E> {
    /// I2C bus error
    I2c(E),

    /// Invalid chip ID was read
    InvalidChipId(u8),
}

#[cfg(feature = "defmt")]
impl<E> Format for Mpu6050Error<E>
where
    E: Format,
{
    fn format(&self, f: defmt::Formatter) {
        match self {
            Mpu6050Error::I2c(e) => defmt::write!(f, "I2c error: {}", e),
            Mpu6050Error::InvalidChipId(id) => defmt::write!(f, "Invalid chip ID: {}", id),
        }
    }
}

/// Handles all operations on/with Mpu6050
pub struct Mpu6050<I> {
    i2c: I,
    slave_addr: u8,
    acc_sensitivity: f32,
    gyro_sensitivity: f32,
    gyro_fine_tune_offsets: Vector3d<i32>,
}

#[cfg(feature = "defmt")]
impl<I, E> Format for Mpu6050<I>
where
    I: I2c<Error = E>,
{
    fn format(&self, f: defmt::Formatter) {
        defmt::write!(
            f,
            "Mpu6050< addr: 0x{:X}, acc_sensitivity: {}, gyro_sensitivity: {} >",
            self.slave_addr,
            self.acc_sensitivity,
            self.gyro_sensitivity
        );
    }
}

impl<I, E> Mpu6050<I>
where
    I: I2c<Error = E>,
{
    /// Side effect free constructor with default sensitivies, no calibration
    pub fn new(i2c: I) -> Self {
        Mpu6050 {
            i2c,
            slave_addr: DEFAULT_SLAVE_ADDR,
            acc_sensitivity: ACCEL_SENS.0,
            gyro_sensitivity: GYRO_SENS.0,
            gyro_fine_tune_offsets: Vector3d::<i32>::default(),
        }
    }

    /// custom sensitivity
    pub fn new_with_sens(i2c: I, arange: AccelRange, grange: GyroRange) -> Self {
        Mpu6050 {
            i2c,
            slave_addr: DEFAULT_SLAVE_ADDR,
            acc_sensitivity: arange.sensitivity(),
            gyro_sensitivity: grange.sensitivity(),
            gyro_fine_tune_offsets: Vector3d::<i32>::default(),
        }
    }

    /// Same as `new`, but the chip address can be specified (e.g. 0x69, if the A0 pin is pulled up)
    pub fn new_with_addr(i2c: I, slave_addr: u8) -> Self {
        Mpu6050 {
            i2c,
            slave_addr,
            acc_sensitivity: ACCEL_SENS.0,
            gyro_sensitivity: GYRO_SENS.0,
            gyro_fine_tune_offsets: Vector3d::<i32>::default(),
        }
    }

    /// Combination of `new_with_sens` and `new_with_addr`
    pub fn new_with_addr_and_sens(
        i2c: I,
        slave_addr: u8,
        arange: AccelRange,
        grange: GyroRange,
    ) -> Self {
        Mpu6050 {
            i2c,
            slave_addr,
            acc_sensitivity: arange.sensitivity(),
            gyro_sensitivity: grange.sensitivity(),
            gyro_fine_tune_offsets: Vector3d::<i32>::default(),
        }
    }

    /// Wakes MPU6050 with all sensors enabled (default)
    fn wake<D: DelayNs>(&mut self, delay: &mut D) -> Result<(), Mpu6050Error<E>> {
        // MPU6050 has sleep enabled by default -> set bit 0 to wake
        // Set clock source to be PLL with x-axis gyroscope reference, bits 2:0 = 001 (See Register Map )
        self.write_byte(PWR_MGMT_1::ADDR, 0x01)?;
        delay.delay_ms(100u32);
        Ok(())
    }

    /// From Register map:
    /// "An  internal  8MHz  oscillator,  gyroscope based  clock,or  external  sources  can  be
    /// selected  as the MPU-60X0 clock source.
    /// When the internal 8 MHz oscillator or an external source is chosen as the clock source,
    /// the MPU-60X0 can operate in low power modes with the gyroscopes disabled. Upon power up,
    /// the MPU-60X0clock source defaults to the internal oscillator. However, it is highly
    /// recommended  that  the  device beconfigured  to  use  one  of  the  gyroscopes
    /// (or  an  external  clocksource) as the clock reference for improved stability.
    /// The clock source can be selected according to the following table...."
    pub fn set_clock_source(&mut self, source: CLKSEL) -> Result<(), Mpu6050Error<E>> {
        Ok(self.write_bits(
            PWR_MGMT_1::ADDR,
            PWR_MGMT_1::CLKSEL.bit,
            PWR_MGMT_1::CLKSEL.length,
            source as u8,
        )?)
    }

    /// get current clock source
    pub fn get_clock_source(&mut self) -> Result<CLKSEL, Mpu6050Error<E>> {
        let source = self.read_bits(
            PWR_MGMT_1::ADDR,
            PWR_MGMT_1::CLKSEL.bit,
            PWR_MGMT_1::CLKSEL.length,
        )?;
        Ok(CLKSEL::from(source))
    }

    /// Init wakes MPU6050 and verifies register addr, e.g. in i2c
    pub fn init<D: DelayNs>(&mut self, delay: &mut D) -> Result<(), Mpu6050Error<E>> {
        self.wake(delay)?;
        self.verify()?;
        self.set_accel_range(AccelRange::G2)?;
        self.set_gyro_range(GyroRange::D250)?;
        self.set_accel_hpf(ACCEL_HPF::_RESET)?;
        Ok(())
    }

    /// Verifies device to address 0x68 with WHOAMI.addr() Register
    fn verify(&mut self) -> Result<(), Mpu6050Error<E>> {
        let address = self.read_byte(WHOAMI)?;
        if address != DEFAULT_SLAVE_ADDR {
            return Err(Mpu6050Error::InvalidChipId(address));
        }
        Ok(())
    }

    /// setup motion detection
    /// sources:
    /// * https://github.com/kriswiner/MPU6050/blob/a7e0c8ba61a56c5326b2bcd64bc81ab72ee4616b/MPU6050IMU.ino#L486
    /// * https://arduino.stackexchange.com/a/48430
    pub fn setup_motion_detection(&mut self) -> Result<(), Mpu6050Error<E>> {
        self.write_byte(0x6B, 0x00)?;
        // optional? self.write_byte(0x68, 0x07)?; // Reset all internal signal paths in the MPU-6050 by writing 0x07 to register 0x68;
        self.write_byte(INT_PIN_CFG::ADDR, 0x20)?; //write register 0x37 to select how to use the interrupt pin. For an active high, push-pull signal that stays until register (decimal) 58 is read, write 0x20.
        self.write_byte(ACCEL_CONFIG::ADDR, 0x01)?; //Write register 28 (==0x1C) to set the Digital High Pass Filter, bits 3:0. For example set it to 0x01 for 5Hz. (These 3 bits are grey in the data sheet, but they are used! Leaving them 0 means the filter always outputs 0.)
        self.write_byte(MOT_THR, 10)?; //Write the desired Motion threshold to register 0x1F (For example, write decimal 20).
        self.write_byte(MOT_DUR, 40)?; //Set motion detect duration to 1  ms; LSB is 1 ms @ 1 kHz rate
        self.write_byte(0x69, 0x15)?; //to register 0x69, write the motion detection decrement and a few other settings (for example write 0x15 to set both free-fall and motion decrements to 1 and accelerometer start-up delay to 5ms total by adding 1ms. )
        self.write_byte(INT_ENABLE::ADDR, 0x40)?; //write register 0x38, bit 6 (0x40), to enable motion detection interrupt.
        Ok(())
    }

    /// get whether or not motion has been detected (INT_STATUS, MOT_INT)
    pub fn get_motion_detected(&mut self) -> Result<bool, Mpu6050Error<E>> {
        Ok(self.read_bit(INT_STATUS::ADDR, INT_STATUS::MOT_INT)? != 0)
    }

    /// set accel high pass filter mode
    pub fn set_accel_hpf(&mut self, mode: ACCEL_HPF) -> Result<(), Mpu6050Error<E>> {
        Ok(self.write_bits(
            ACCEL_CONFIG::ADDR,
            ACCEL_CONFIG::ACCEL_HPF.bit,
            ACCEL_CONFIG::ACCEL_HPF.length,
            mode as u8,
        )?)
    }

    /// get accel high pass filter mode
    pub fn get_accel_hpf(&mut self) -> Result<ACCEL_HPF, Mpu6050Error<E>> {
        let mode: u8 = self.read_bits(
            ACCEL_CONFIG::ADDR,
            ACCEL_CONFIG::ACCEL_HPF.bit,
            ACCEL_CONFIG::ACCEL_HPF.length,
        )?;

        Ok(ACCEL_HPF::from(mode))
    }

    /// Set gyro range, and update sensitivity accordingly
    pub fn set_gyro_range(&mut self, range: GyroRange) -> Result<(), Mpu6050Error<E>> {
        self.write_bits(
            GYRO_CONFIG::ADDR,
            GYRO_CONFIG::FS_SEL.bit,
            GYRO_CONFIG::FS_SEL.length,
            range as u8,
        )?;

        self.gyro_sensitivity = range.sensitivity();
        Ok(())
    }

    /// get current gyro range
    pub fn get_gyro_range(&mut self) -> Result<GyroRange, Mpu6050Error<E>> {
        let byte = self.read_bits(
            GYRO_CONFIG::ADDR,
            GYRO_CONFIG::FS_SEL.bit,
            GYRO_CONFIG::FS_SEL.length,
        )?;

        Ok(GyroRange::from(byte))
    }

    /// set accel range, and update sensitivy accordingly
    pub fn set_accel_range(&mut self, range: AccelRange) -> Result<(), Mpu6050Error<E>> {
        self.write_bits(
            ACCEL_CONFIG::ADDR,
            ACCEL_CONFIG::FS_SEL.bit,
            ACCEL_CONFIG::FS_SEL.length,
            range as u8,
        )?;

        self.acc_sensitivity = range.sensitivity();
        Ok(())
    }

    /// get current accel_range
    pub fn get_accel_range(&mut self) -> Result<AccelRange, Mpu6050Error<E>> {
        let byte = self.read_bits(
            ACCEL_CONFIG::ADDR,
            ACCEL_CONFIG::FS_SEL.bit,
            ACCEL_CONFIG::FS_SEL.length,
        )?;

        Ok(AccelRange::from(byte))
    }

    /// reset device
    pub fn reset_device<D: DelayNs>(&mut self, delay: &mut D) -> Result<(), Mpu6050Error<E>> {
        self.write_bit(PWR_MGMT_1::ADDR, PWR_MGMT_1::DEVICE_RESET, true)?;
        delay.delay_ms(100u32);
        // Note: Reset sets sleep to true! Section register map: resets PWR_MGMT to 0x40
        Ok(())
    }

    /// enable, disable sleep of sensor
    pub fn set_sleep_enabled(&mut self, enable: bool) -> Result<(), Mpu6050Error<E>> {
        Ok(self.write_bit(PWR_MGMT_1::ADDR, PWR_MGMT_1::SLEEP, enable)?)
    }

    /// get sleep status
    pub fn get_sleep_enabled(&mut self) -> Result<bool, Mpu6050Error<E>> {
        Ok(self.read_bit(PWR_MGMT_1::ADDR, PWR_MGMT_1::SLEEP)? != 0)
    }

    /// enable, disable temperature measurement of sensor
    /// TEMP_DIS actually saves "disabled status"
    /// 1 is disabled! -> enable=true : bit=!enable
    pub fn set_temp_enabled(&mut self, enable: bool) -> Result<(), Mpu6050Error<E>> {
        Ok(self.write_bit(PWR_MGMT_1::ADDR, PWR_MGMT_1::TEMP_DIS, !enable)?)
    }

    /// get temperature sensor status
    /// TEMP_DIS actually saves "disabled status"
    /// 1 is disabled! -> 1 == 0 : false, 0 == 0 : true
    pub fn get_temp_enabled(&mut self) -> Result<bool, Mpu6050Error<E>> {
        Ok(self.read_bit(PWR_MGMT_1::ADDR, PWR_MGMT_1::TEMP_DIS)? == 0)
    }

    /// set accel x self test
    pub fn set_accel_x_self_test(&mut self, enable: bool) -> Result<(), Mpu6050Error<E>> {
        Ok(self.write_bit(ACCEL_CONFIG::ADDR, ACCEL_CONFIG::XA_ST, enable)?)
    }

    /// get accel x self test
    pub fn get_accel_x_self_test(&mut self) -> Result<bool, Mpu6050Error<E>> {
        Ok(self.read_bit(ACCEL_CONFIG::ADDR, ACCEL_CONFIG::XA_ST)? != 0)
    }

    /// set accel y self test
    pub fn set_accel_y_self_test(&mut self, enable: bool) -> Result<(), Mpu6050Error<E>> {
        Ok(self.write_bit(ACCEL_CONFIG::ADDR, ACCEL_CONFIG::YA_ST, enable)?)
    }

    /// get accel y self test
    pub fn get_accel_y_self_test(&mut self) -> Result<bool, Mpu6050Error<E>> {
        Ok(self.read_bit(ACCEL_CONFIG::ADDR, ACCEL_CONFIG::YA_ST)? != 0)
    }

    /// set accel z self test
    pub fn set_accel_z_self_test(&mut self, enable: bool) -> Result<(), Mpu6050Error<E>> {
        Ok(self.write_bit(ACCEL_CONFIG::ADDR, ACCEL_CONFIG::ZA_ST, enable)?)
    }

    /// get accel z self test
    pub fn get_accel_z_self_test(&mut self) -> Result<bool, Mpu6050Error<E>> {
        Ok(self.read_bit(ACCEL_CONFIG::ADDR, ACCEL_CONFIG::ZA_ST)? != 0)
    }

    /// Roll and pitch estimation from raw accelerometer readings
    /// NOTE: no yaw! no magnetometer present on MPU6050
    /// https://www.nxp.com/docs/en/application-note/AN3461.pdf equation 28, 29
    pub fn get_acc_angles(&mut self) -> Result<Vector2d<f32>, Mpu6050Error<E>> {
        let acc = self.get_acc()?;

        Ok(Vector2d::<f32> {
            // x: atan2f(acc.y, sqrtf(powf(acc.x, 2.) + powf(acc.z, 2.))),
            // y: atan2f(-acc.x, sqrtf(powf(acc.y, 2.) + powf(acc.z, 2.)))
            x: acc.y.atan2((acc.x.powf(2.) + acc.z.powf(2.)).sqrt()),
            y: (-acc.x).atan2((acc.y.powf(2.) + acc.z.powf(2.)).sqrt()),
        })
    }

    /// Converts 2 bytes number in 2 compliment
    /// TODO i16?! whats 0x8000?!
    fn read_word_2c(&self, byte: &[u8]) -> i32 {
        let high: i32 = byte[0] as i32;
        let low: i32 = byte[1] as i32;
        let mut word: i32 = (high << 8) + low;

        if word >= 0x8000 {
            word = -((65535 - word) + 1);
        }

        word
    }

    /// Reads rotation (gyro/acc) from specified register returning as Vector3s<i32>
    fn read_rot_i32(&mut self, reg: u8) -> Result<Vector3d::<i32>, Mpu6050Error<E>> {
        let mut buf: [u8; 6] = [0; 6];
        self.read_bytes(reg, &mut buf)?;

        Ok(Vector3d::<i32> {
            x: self.read_word_2c(&buf[0..2]) + self.gyro_fine_tune_offsets.x,  // x
            y: self.read_word_2c(&buf[2..4]) + self.gyro_fine_tune_offsets.y,  // y
            z: self.read_word_2c(&buf[4..6]) + self.gyro_fine_tune_offsets.z,  // z
        })
    }

    /// Reads rotation (gyro/acc) from specified register
    fn read_rot(&mut self, reg: u8) -> Result<Vector3d<f32>, Mpu6050Error<E>> {
        // convert i32 to Vector3d<f32>
        let i32vec = self.read_rot_i32(reg)?;
        Ok(Vector3d::<f32> {
            x: i32vec.x as f32,
            y: i32vec.y as f32,
            z: i32vec.z as f32,
        })
    }

    /// Accelerometer readings in g
    pub fn get_acc(&mut self) -> Result<Vector3d<f32>, Mpu6050Error<E>> {
        let mut acc = self.read_rot(ACC_REGX_H)?;

        acc *= 1.0 / self.acc_sensitivity;

        Ok(acc)
    }

    /// Gyro readings in rad/s
    pub fn get_gyro(&mut self) -> Result<Vector3d<f32>, Mpu6050Error<E>> {
        let mut gyro = self.get_gyro_deg()?;

        gyro *= PI_180;

        Ok(gyro)
    }

    /// Gyro readings in deg/s
    pub fn get_gyro_deg(&mut self) -> Result<Vector3d<f32>, Mpu6050Error<E>> {
        let mut gyro = self.read_rot(GYRO_REGX_H)?;

        gyro *= 1.0 / self.gyro_sensitivity;

        Ok(gyro)
    }

    /// Sensor Temp in degrees celcius
    pub fn get_temp(&mut self) -> Result<f32, Mpu6050Error<E>> {
        let mut buf: [u8; 2] = [0; 2];
        self.read_bytes(TEMP_OUT_H, &mut buf)?;
        let raw_temp = self.read_word_2c(&buf[0..2]) as f32;

        // According to revision 4.2
        Ok((raw_temp / TEMP_SENSITIVITY) + TEMP_OFFSET)
    }

    /// get gyro offsets
    pub fn get_gyro_offsets(&mut self) -> Result<Vector3d<i32>, Mpu6050Error<E>> {
        let mut buf: [u8; 2] = [0; 2];
        let mut offsets: Vector3d<i32> = Vector3d::<i32>::default();

        self.read_bytes(XG_OFFS_USRH, &mut buf)?;
        offsets.x = self.read_word_2c(&buf[0..2]);
        self.read_bytes(YG_OFFS_USRH, &mut buf)?;
        offsets.y = self.read_word_2c(&buf[0..2]);
        self.read_bytes(ZG_OFFS_USRH, &mut buf)?;
        offsets.z = self.read_word_2c(&buf[0..2]);

        Ok(offsets)
    }

    /// set gyro offsets
    pub fn set_gyro_offsets(&mut self, x_offset: i16, y_offset: i16, z_offset: i16) -> Result<(), Mpu6050Error<E>> {
        #[cfg(feature = "defmt")]
        debug!("Setting gyro offsets: x: {}, y: {}, z: {}", x_offset, y_offset, z_offset);
        self.write_word(XG_OFFS_USRH, x_offset as u16)?;
        self.write_word(YG_OFFS_USRH, y_offset as u16)?;
        self.write_word(ZG_OFFS_USRH, z_offset as u16)?;
        Ok(())
    }

    /// Calibrate gyro and update offsets
    /// To calibrate the gyro, the sensor must be stationary and level. The sensor should be placed on a flat, level surface.
    pub fn calibrate_gyro<D: DelayNs, F: FnMut(usize)>(&mut self, delay: &mut D, mut callback: F) -> Result<(), Mpu6050Error<E>> {
        const MAX_CALIBRATION_STEPS: usize = 20;
        // the measurement mean is in raw units (Count)/°/s. The target is to get it as close to 0 as possible, but it is not possible to get it to 0.
        // we will aim for getting withing 1.5 counts/°/s to 0. For a 250°/s range, this is ~0.011 °/s error
        const TARGET_MAX_MEASUREMENT_MEAN: f32 = 1.5;

        #[cfg(feature = "defmt")]
        info!("Calibrating gyro");

        // first set current offsets to 0
        self.set_gyro_offsets(0, 0, 0)?;
        self.gyro_fine_tune_offsets = Vector3d::<i32>::default();

        let mut offsets_found = false;
        let mut calibration_step: usize = 0;
        while !offsets_found && calibration_step < MAX_CALIBRATION_STEPS {
            // get mean gyro readings
            let mean = self.calibrate_gyro_mean_sensor(delay)?;

            // calculate new offsets. To converge on the right offsets, we take the current offset
            // and substract the the mean/4. This is repeated until the mean is close to 0 or we
            // reach 20 iterations
            let offsets = self.get_gyro_offsets()?;
            let mut updated_offsets = offsets.clone();
            if mean.x.abs() > TARGET_MAX_MEASUREMENT_MEAN {
                updated_offsets.x = offsets.x - (mean.x.signum()*f32::max(mean.x.abs()/4.0, 1.0)) as i32;
            }
            if mean.y.abs() > TARGET_MAX_MEASUREMENT_MEAN {
                updated_offsets.y = offsets.y - (mean.y.signum()*f32::max(mean.y.abs()/4.0, 1.0)) as i32;
            }
            if mean.z.abs() > TARGET_MAX_MEASUREMENT_MEAN {
                updated_offsets.z = offsets.z - (mean.z.signum()*f32::max(mean.z.abs()/4.0, 1.0)) as i32;
            }
            self.set_gyro_offsets(
                updated_offsets.x as i16,
                updated_offsets.y as i16,
                updated_offsets.z as i16,
            )?;

            #[cfg(feature = "defmt")]
            info!(
                "Calibration step: {}\n  Mean: x = {}, y  = {}, z = {}\n  Found Offsets: x = {}, y  = {}, z = {}",
                calibration_step, mean.x, mean.y, mean.z, updated_offsets.x, updated_offsets.y, updated_offsets.z
            );
            // callback is any
            callback(calibration_step);

            // determine if we are done
            if mean.x.abs() < TARGET_MAX_MEASUREMENT_MEAN && mean.y.abs() < TARGET_MAX_MEASUREMENT_MEAN && mean.z.abs() < TARGET_MAX_MEASUREMENT_MEAN {
                offsets_found = true;
                // the mean values we still get here are the error in the sensor. We can use this to fine tune the sensor beyond the
                // offsets we found.
                self.gyro_fine_tune_offsets =  Vector3d::<i32> {
                    x: -mean.x as i32,
                    y: -mean.y as i32,
                    z: -mean.z as i32,
                };

                #[cfg(feature = "defmt")]
                info!(
                    "Calibration done. Fine tune offsets: x = {}, y  = {}, z = {}",
                    self.gyro_fine_tune_offsets.x, self.gyro_fine_tune_offsets.y, self.gyro_fine_tune_offsets.z
                );
            }
            calibration_step += 1;
        }

        Ok(())
    }

    fn calibrate_gyro_mean_sensor<D: DelayNs>(&mut self, delay: &mut D) -> Result<Vector3d<f32>, Mpu6050Error<E>> {
        const MEASURMENT_COUNT: i32 = 1000;
        let mut sum: Vector3d<i32> = Vector3d::<i32>::default();

        // discard first 100 readings
        for _ in 0..100 {
            let _ = self.read_rot_i32(GYRO_REGX_H)?;
            delay.delay_ms(2u32);
        }
        for _ in 0..MEASURMENT_COUNT {
            let gyro = self.read_rot_i32(GYRO_REGX_H)?;

            sum += gyro;
            delay.delay_ms(2u32);
        }
        let mean = Vector3d::<f32> {
            x: sum.x as f32 / MEASURMENT_COUNT as f32,
            y: sum.y as f32 / MEASURMENT_COUNT as f32,
            z: sum.z as f32 / MEASURMENT_COUNT as f32,
        };
        Ok(mean)
    }

    pub fn write_word(&mut self, reg: u8, word_value: u16) -> Result<(), Mpu6050Error<E>> {
        let data = [reg, (word_value >> 8) as u8, (word_value & 0x00FF) as u8];
        self.i2c.write(self.slave_addr, &data)
           .map_err(Mpu6050Error::I2c)?;
        // delay disabled for dev build
        // TODO: check effects with physical unit
        // self.delay.delay_ms(10u8);
        Ok(())
    }

    /// Writes byte to register
    pub fn write_byte(&mut self, reg: u8, byte: u8) -> Result<(), Mpu6050Error<E>> {
        self.i2c.write(self.slave_addr, &[reg, byte])
           .map_err(Mpu6050Error::I2c)?;
        // delay disabled for dev build
        // TODO: check effects with physical unit
        // self.delay.delay_ms(10u8);
        Ok(())
    }

    /// Enables bit n at register address reg
    pub fn write_bit(&mut self, reg: u8, bit_n: u8, enable: bool) -> Result<(), Mpu6050Error<E>> {
        let mut byte: [u8; 1] = [0; 1];
        self.read_bytes(reg, &mut byte)?;
        bits::set_bit(&mut byte[0], bit_n, enable);
        Ok(self.write_byte(reg, byte[0])?)
    }

    /// Write bits data at reg from start_bit to start_bit+length
    pub fn write_bits(
        &mut self,
        reg: u8,
        start_bit: u8,
        length: u8,
        data: u8,
    ) -> Result<(), Mpu6050Error<E>> {
        let mut byte: [u8; 1] = [0; 1];
        self.read_bytes(reg, &mut byte)?;
        bits::set_bits(&mut byte[0], start_bit, length, data);
        Ok(self.write_byte(reg, byte[0])?)
    }

    /// Read bit n from register
    fn read_bit(&mut self, reg: u8, bit_n: u8) -> Result<u8, Mpu6050Error<E>> {
        let mut byte: [u8; 1] = [0; 1];
        self.read_bytes(reg, &mut byte)?;
        Ok(bits::get_bit(byte[0], bit_n))
    }

    /// Read bits at register reg, starting with bit start_bit, until start_bit+length
    pub fn read_bits(&mut self, reg: u8, start_bit: u8, length: u8) -> Result<u8, Mpu6050Error<E>> {
        let mut byte: [u8; 1] = [0; 1];
        self.read_bytes(reg, &mut byte)?;
        Ok(bits::get_bits(byte[0], start_bit, length))
    }

    /// Reads byte from register
    pub fn read_byte(&mut self, reg: u8) -> Result<u8, Mpu6050Error<E>> {
        let mut byte: [u8; 1] = [0; 1];
        self.i2c.write_read(self.slave_addr, &[reg], &mut byte)
            .map_err(Mpu6050Error::I2c)?;
        Ok(byte[0])
    }

    /// Reads series of bytes into buf from specified reg
    pub fn read_bytes(&mut self, reg: u8, buf: &mut [u8]) -> Result<(), Mpu6050Error<E>> {
        self.i2c.write_read(self.slave_addr, &[reg], buf)
            .map_err(Mpu6050Error::I2c)?;
        Ok(())
    }
}
