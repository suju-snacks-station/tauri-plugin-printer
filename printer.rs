use std::time::Duration;
use tokio::io::AsyncWriteExt;
use serialport;
use std::process::Command;
use std::env;
use winapi::um::winspool;
use std::ffi::CString;
use std::ptr;
use crate::db::{DbState, PrinterSettings, DailySalesReport, Error};
use chrono::{Local, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::Write; 


const PRINT_TIMEOUT: Duration = Duration::from_secs(10);
const USB_WRITE_DELAY: Duration = Duration::from_millis(100);

#[tauri::command]
pub async fn print_to_all_printers(
    order_id: i64,
    content: String,
    printer_settings: PrinterSettings,
    state: tauri::State<'_, DbState>,
) -> Result<String, Error> {
    if content.is_empty() {
        log::error!("Print content cannot be empty");
        return Err(Error::Printer("Print content cannot be empty".into()));
    }

    let errors = validate_printer_settings(&printer_settings);
    if !errors.is_empty() {
        let error_msg = errors.join(" | ");
        log::error!("Invalid printer settings: {}", error_msg);
        return Err(Error::Printer(error_msg));
    }

    let mut print_errors = Vec::new();

    // USB printing
    if !printer_settings.usb_port.is_empty() {
        match attempt_usb_print(&content, &printer_settings).await {
            Ok(_) => {
                log::info!("USB print successful for order {}", order_id);
                let conn = state.0.lock().map_err(|e| e.to_string())?;
                if let Err(e) = set_print_status_internal(&conn, order_id, "usb", true){
                    log::error!("Failed to update USB print status: {}", e);
                }
            },
            Err(e) => {
                log::error!("USB Printer Error for order {}: {}", e, order_id);
                print_errors.push(format!("USB: {}", e));
            }
        }
    }

    // Network printing
    if !printer_settings.network_ip.is_empty() {
        match attempt_network_print(&content, &printer_settings).await {
            Ok(_) => {
                log::info!("Network print successful for order {}", order_id);
                let conn = state.0.lock().map_err(|e| e.to_string())?;
                if let Err(e) = set_print_status_internal(&conn, order_id, "network", true){
                    log::error!("Failed to update Network print status: {}", e);
                }
            }
            Err(e) => {
                log::error!("Network Printer Error for order {}: {}", e, order_id);
                print_errors.push(format!("Network: {}", e));
            }
        }
    }

    if print_errors.is_empty() {
       Ok("Successfully sent print job(s).".to_string())
    } else {
       Err(Error::Printer(print_errors.join(" | ")))
    }
}

// 
#[tauri::command]
pub async fn generate_kot_content_from_db(order_id: i64, is_reprint: bool, username: String, state: tauri::State<'_, DbState>,) -> Result<String, Error> {
    let conn = state.0.lock().map_err(|e| Error::Lock(e.to_string()))?;

    // ESC/POS Commands
    const INIT: &str = "\x1B@";
    const BOLD_ON: &str = "\x1B\x45\x01";
    const BOLD_OFF: &str = "\x1B\x45\x00";
    const CUT_PAPER: &str = "\x1D\x56\x41\x00";
    const LINE_WIDTH: usize = 48;

    // 1. Fetch order details
    ......

    // Build the content string
    let mut content = String::new();
    content.push_str(INIT);

    if is_reprint {
        content.push_str(&format!("{}*** REPRINT ***{}\n", BOLD_ON, BOLD_OFF));
    }

    let order_type_text = if has_table { "Table " } else { "[Pack]" };
    let date_time = Local::now().format("%Y-%m-%d %I:%M:%S %p").to_string();
    let kot_number = order_number.split('-').last().unwrap_or("");
    let header_line = format!(
        "Kot: {}{}{}{}{}{}{}",
        BOLD_ON, kot_number, BOLD_OFF,
        " ".repeat(6),
        BOLD_ON, order_type_text, BOLD_OFF
    );
    content.push_str(&format!("{}{}{}\n", header_line, " ".repeat(6), date_time));
    content.push_str(&("-".repeat(LINE_WIDTH) + "\n"));
    content.push_str(&format!("Notes: {}\n", notes));
    content.push_str(&("-".repeat(LINE_WIDTH) + "\n"));

    // --- Render Items ---
    for (item_type, name, quantity, dinein_json, pack_json) in &item_data {
        content.push_str(&format!("{}{}) {}{}\n", BOLD_ON, quantity, name, BOLD_OFF));

        match item_type.as_str() {
            "corndog" | "beverage" => {
                let render_complex_section = |json_str: &Option<String>, section_name: &str, content_str: &mut String| {
                    if let Some(json) = json_str {
                        if let Ok(data) = serde_json::from_str::<SectionData>(json) {
                            if data.total > 0 {
                                *content_str += &format!("  - {} ({})\n", section_name, data.total);
                                for (flavor_name, flavor_data) in &data.flavors {
                                    if flavor_data.total > 0 {
                                        let mut modifier_texts = Vec::new();
                                        for (mod_key, mod_val) in &flavor_data.modifier {
                                            if *mod_val > 0 {
                                                modifier_texts.push(format!("{}:{}", mod_key.replace("_", " "), mod_val));
                                            }
                                        }
                                        *content_str += &format!("    - {}: {}", flavor_name.replace("_", " "), flavor_data.total);
                                        if !modifier_texts.is_empty() {
                                            *content_str += &format!(" ({})\n", modifier_texts.join(", "));
                                        } else {
                                            *content_str += "\n";
                                        }
                                    }
                                }
                            }
                        }
                    }
                };
                render_complex_section(dinein_json, "Table", &mut content);
                render_complex_section(pack_json, "Pack", &mut content);
            }
            _ => { // For simple items like addons and sausages
                let render_simple_section = |json_str: &Option<String>, section_name: &str, content_str: &mut String| {
                    if let Some(json) = json_str {
                        if let Ok(data) = serde_json::from_str::<SimpleSectionData>(json) {
                            if data.total > 0 {
                                *content_str += &format!("  - {}: {}\n", section_name, data.total);
                            }
                        }
                    }
                };
                render_simple_section(dinein_json, "Table", &mut content);
                render_simple_section(pack_json, "Pack", &mut content);
            }
        }
    }

    // --- Footer ---
    content.push_str(&("-".repeat(LINE_WIDTH) + "\n"));
    let estimate_text = if discount_amount > 0.0 {
        format!("{} (-{})", total_amount, discount_amount)
    } else {
        format!("{}", total_amount)
    };
    let footer_padding = LINE_WIDTH.saturating_sub(estimate_text.len()).saturating_sub(username.len());
    content.push_str(&format!("{}{}{}\n", estimate_text, " ".repeat(footer_padding), username));
    content.push_str("Note: This is not a bill. Please contact cash counter for the bill.");
    content.push_str("\n\n");
    content.push_str(CUT_PAPER);

    Ok(content)
}

fn validate_printer_settings(settings: &PrinterSettings) -> Vec<String> {
    let mut errors = Vec::new();
    
    if settings.usb_port.is_empty() && settings.network_ip.is_empty() {
        errors.push("No printers configured".to_string());
    }
    
    if !settings.usb_port.is_empty() && settings.baud_rate == 0 {
        errors.push("Invalid baud rate for USB printer".to_string());
    }
    
    errors
}

async fn attempt_usb_print(content: &str, settings: &PrinterSettings) -> Result<(), String> {
    // Try Windows RAW printing first
    match try_raw_usb_print(content, settings).await {
        Ok(_) => return Ok(()),
         Err(e) => log::error!("Raw USB print failed: {}", e),
    }

    // Fall back to Windows print command
    match try_windows_print_command(content, &settings.usb_port).await {
        Ok(_) => return Ok(()),
        Err(e) => log::error!("Windows print command failed: {}", e),
    }
    // Fall back to serial port
    if settings.baud_rate > 0 {
        match try_serial_port(content, settings).await {
            Ok(_) => return Ok(()),
            Err(e) => log::warn!("Serial port print failed. Error: {}", e),
        }
    }

    Err("All USB printing methods failed".to_string())
}

async fn try_raw_usb_print(content: &str, settings: &PrinterSettings) -> Result<(), String> {
    let printer_name = CString::new(settings.usb_port.clone()).map_err(|e| format!("Invalid printer name: {}", e))?;
    let mut hprinter = ptr::null_mut();

    unsafe {
        if winspool::OpenPrinterA(printer_name.as_ptr() as *mut _, &mut hprinter, ptr::null_mut()) == 0 {
            return Err(format!("OpenPrinter failed with error code: {}", winapi::um::errhandlingapi::GetLastError()));
        }

        let doc_name = CString::new("KOT Print").unwrap();
        let data_type = CString::new("RAW").unwrap();
        
        let doc_info = winspool::DOC_INFO_1A {
            pDocName: doc_name.as_ptr() as *mut _,
            pOutputFile: ptr::null_mut(),
            pDatatype: data_type.as_ptr() as *mut _,
        };

        if winspool::StartDocPrinterA(hprinter, 1, &doc_info as *const _ as *mut _) == 0 {
            winspool::ClosePrinter(hprinter);
            return Err(format!("StartDocPrinter failed: {}", winapi::um::errhandlingapi::GetLastError()));
        }

        let mut bytes_written: u32 = 0;
        if winspool::WritePrinter(hprinter, content.as_ptr() as *mut _, content.len() as u32, &mut bytes_written) == 0 {
            winspool::EndDocPrinter(hprinter);
            winspool::ClosePrinter(hprinter);
            return Err(format!("WritePrinter failed: {}", winapi::um::errhandlingapi::GetLastError()));
        }

        winspool::EndDocPrinter(hprinter);
        winspool::ClosePrinter(hprinter);
    }

    Ok(())
}

async fn try_windows_print_command(content: &str, printer_name: &str) -> Result<(), String> {
    let temp_path = env::temp_dir().join("zkp_print.txt");
    let formatted_content = format!("\x1B@{}", content);
    
    if let Err(e) = std::fs::write(&temp_path, formatted_content) {
        log::error!("Failed to create print file: {}", e);
        return Err(format!("Failed to create print file: {}", e));
    }

    let output = match Command::new("cmd")
        .args(&["/C", "print", &format!("/D:\\\\localhost\\{}", printer_name), 
                temp_path.to_str().unwrap()])
        .output()
    {
        Ok(output) => output,
        Err(e) => {
            log::error!("Failed to execute print command: {}", e);
            let _ = std::fs::remove_file(&temp_path);
            return Err(format!("Failed to execute print command: {}", e));
        }
    };

    let _ = std::fs::remove_file(&temp_path);

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        log::error!("Print command failed. Status: {}. Stderr: {}. Stdout: {}", output.status, stderr, stdout);
        return Err(format!("Print command failed. Status: {}. Stderr: {}. Stdout: {}", output.status, stderr, stdout));
    }

    Ok(())
}

async fn try_serial_port(content: &str, settings: &PrinterSettings) -> Result<(), String> {
    let port_name = &settings.usb_port;
    let baud_rate = settings.baud_rate;
    
    let mut port = serialport::new(port_name, baud_rate)
        .timeout(PRINT_TIMEOUT)
        .open()
        .map_err(|e| format!("Failed to open serial port {}: {}", port_name, e))?;

    port.write_all(content.as_bytes()).map_err(|e| format!("Failed to write to port {}: {}", port_name, e))?;
    port.flush().map_err(|e| format!("Failed to flush port {}: {}", port_name, e))?;
    
    tokio::time::sleep(USB_WRITE_DELAY).await;
    
    Ok(())
}

async fn attempt_network_print(content: &str, settings: &PrinterSettings) -> Result<(), String> {
    use tokio::{net::TcpStream, time::timeout};
    
    let stream_result = timeout(PRINT_TIMEOUT, TcpStream::connect(&settings.network_ip)).await;
    
    let mut stream = match stream_result {
        Ok(Ok(stream)) => stream,
        Ok(Err(e)) => return Err(format!("Connection failed: {}", e)),
        Err(_) => return Err("Connection timeout".to_string()),
    };

    stream.write_all(content.as_bytes()).await.map_err(|e| format!("Write failed: {}", e))?;
    stream.flush().await.map_err(|e| format!("Flush failed: {}", e))?;

    Ok(())
}

}
