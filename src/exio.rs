use esp_idf_svc::hal::{
    self,
    gpio::{Gpio10, Gpio11},
    i2c::{I2cDriver, I2C0},
};

#[derive(Clone, Copy)]
pub enum ExioPin {
    Exio1,
    Exio2,
    Exio3,
    Exio4,
    Exio5,
    Exio6,
    Exio7,
    Exio8,
}

const TCA9554_ADDRESS: u8 = 0x20;
const TCA9554_INPUT_REG: u8 = 0x00;
const TCA9554_OUTPUT_REG: u8 = 0x01;
const TCA9554_POLARITY_REG: u8 = 0x02;
const TCA9554_CONFIG_REG: u8 = 0x03;

pub fn i2c_init(
    i2c: I2C0<'static>,
    sda: Gpio11<'static>,
    scl: Gpio10<'static>,
) -> anyhow::Result<I2cDriver<'static>> {
    const I2C_MASTER_FREQ_HZ: u32 = 400000;
    let config = esp_idf_svc::hal::i2c::config::Config::new()
        .scl_enable_pullup(true)
        .sda_enable_pullup(true)
        .baudrate(hal::units::Hertz(I2C_MASTER_FREQ_HZ));
    let driver = esp_idf_svc::hal::i2c::I2cDriver::new(i2c, sda, scl, &config)?;
    Ok(driver)
}

fn read_reg(i2c: &mut I2cDriver<'static>, reg: u8) -> anyhow::Result<u8> {
    let mut data = [0; 1];
    i2c.write_read(TCA9554_ADDRESS, &[reg], &mut data, 100)?;
    Ok(data[0])
}

fn write_reg(i2c: &mut I2cDriver<'static>, reg: u8, value: u8) -> anyhow::Result<()> {
    const PORT_TICK_PERIOD_MS: u32 = 1000 / esp_idf_svc::sys::configTICK_RATE_HZ;

    i2c.write(TCA9554_ADDRESS, &[reg, value], 1000 / PORT_TICK_PERIOD_MS)?;
    Ok(())
}

pub fn read_exio(i2c: &mut I2cDriver<'static>, pin: ExioPin) -> anyhow::Result<bool> {
    let input_bits = read_exios(i2c)?;
    let bit_status = (input_bits >> pin as u8) & 0x01;
    Ok(bit_status == 1)
}

pub fn read_exios(i2c: &mut I2cDriver<'static>) -> anyhow::Result<u8> {
    read_reg(i2c, TCA9554_INPUT_REG)
}

pub fn set_exio(i2c: &mut I2cDriver<'static>, pin: ExioPin, state: bool) -> anyhow::Result<()> {
    let mut data = read_reg(i2c, TCA9554_OUTPUT_REG)?;
    let mask = pin as u8;
    if state {
        data |= 1 << mask;
    } else {
        data &= !(1 << mask);
    }
    write_reg(i2c, TCA9554_OUTPUT_REG, data)?;
    Ok(())
}

pub fn set_exios(i2c: &mut I2cDriver<'static>, pin_state: u8) -> anyhow::Result<()> {
    write_reg(i2c, TCA9554_OUTPUT_REG, pin_state)?;
    Ok(())
}

pub fn toggle_exio(i2c: &mut I2cDriver<'static>, pin: ExioPin) -> anyhow::Result<()> {
    let bit_status = read_exio(i2c, pin)?;
    set_exio(i2c, pin, !bit_status)?;
    Ok(())
}

pub fn exio_init(i2c: &mut I2cDriver<'static>) -> anyhow::Result<()> {
    write_reg(i2c, TCA9554_CONFIG_REG, 0x00)
}
