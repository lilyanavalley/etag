/* nRF52840 memory map (no SoftDevice)
 *
 * Flash : 1 MiB  @ 0x0000_0000
 * RAM   : 256 KiB @ 0x2000_0000
 *
 * If you later add the S140 SoftDevice, change to:
 *   FLASH : ORIGIN = 0x00027000, LENGTH = 868K
 *   RAM   : ORIGIN = 0x20020000, LENGTH = 128K
 */
MEMORY
{
    FLASH : ORIGIN = 0x00000000, LENGTH = 1024K
    RAM   : ORIGIN = 0x20000000, LENGTH = 256K
}
