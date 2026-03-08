use eframe::egui;
use sysinfo::{System, ProcessesToUpdate};
use egui_extras::{TableBuilder, Column};
use windows::Win32::System::Threading::{OpenProcess, SetPriorityClass, SetProcessAffinityMask,
                                        GetCurrentProcess, OpenProcessToken,
                                        PROCESS_SET_INFORMATION, HIGH_PRIORITY_CLASS};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES,
                               SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::core::w;

// The exact structure Windows uses to track page lists
#[repr(C)]
#[derive(Default)]
pub struct SystemMemoryListInformation {
    pub zero_page_count: usize,
    pub free_page_count: usize,
    pub modified_page_count: usize,
    pub modified_no_write_page_count: usize,
    pub bad_page_count: usize,
    pub page_count_by_priority: [usize; 8],
    pub repurposed_pages_by_priority: [usize; 8],
    pub modified_page_count_page_file: usize,
}

// Manually link the undocumented NtSetSystemInformation function
#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtSetSystemInformation(
        system_information_class: u32,
        system_information: *const std::ffi::c_void,
        system_information_length: u32,
    ) -> i32; // Returns an NTSTATUS

    fn NtQueryTimerResolution (
        maximum_time: *mut u32,
        minimum_time: *mut u32,
        current_time: *mut u32,
    ) -> i32;

    fn NtSetTimerResolution (
        desired_time: u32,
        set_resolution: u8,
        actual_time: *mut u32,
    ) -> i32;

    fn NtQuerySystemInformation(
        system_information_class: u32,
        system_information: *mut std::ffi::c_void,
        system_information_length: u32,
        return_length: *mut u32,
    ) -> i32;
}
enum ActiveTab {
    Processes,
    MemoryCleaner,
}

#[derive(PartialEq)]
enum SortColumn {
    Pid,
    Name,
    Cpu,
    Ram,
    Disk,
}

struct OptimizerApp {
    search_query: String,
    system: System,
    active_tab: ActiveTab,
    // RAM Settings
    purge_at_standby_mb: u64,
    purge_at_free_mb: u64,
    timer_res_ms: f32,
    polling_rate_ms: u64,
    last_status: String,
    is_auto_purge_enabled: bool,
    sort_column: SortColumn,
    sort_ascending: bool,
    last_refresh: std::time::Instant,
}

// Set starting values on first open
impl Default for OptimizerApp {
    fn default() -> Self {
        // Enable SeProfileSingleProcessPrivilege
        // Strictly required to clear the memory list
        if let Err(e) = enable_privilege(w!("SeProfileSingleProcessPrivilege")) {
            eprintln!("Failed to enable privilege. Are you running as Admin? Error: {}", e);
        }

        // Create the system handle and refresh it to get the first batch of data
        let mut sys = System::new_all();
        sys.refresh_all();

        Self {
            search_query: "".to_owned(),
            system: sys,
            active_tab: ActiveTab::Processes,
            purge_at_standby_mb: 1024, // 1GB Default
            purge_at_free_mb: 1024,    // 1GB Default
            timer_res_ms: 0.5,         // Sweet spot
            polling_rate_ms: 500,     // 0.5 seconds
            last_status: "Ready".to_string(),
            is_auto_purge_enabled: false,
            sort_column: SortColumn::Ram,
            sort_ascending: false,
            last_refresh: std::time::Instant::now(),
        }
    }
}

// Logic
impl OptimizerApp {
    // Calculates the mask that selects only the first thread of every physical core
    fn get_physical_core_mask(&self) -> usize {
        let core_count = self.system.cpus().len(); // Total logical threads
        let mut mask: usize = 0;

        for i in 0..core_count {
            // Only set the bit if it's an even index
            if i % 2 == 0 {
                mask |= 1 << i;
            }
        }
        mask
    }

    fn ui_processes(&mut self, ui: &mut egui::Ui) {
        let using_cpu = self.system.cpus().first().map(|c| c.brand()).unwrap_or("Unknown CPU");
        let thread_count = self.system.cpus().len();

        let process_count = self.system.processes().len();
        ui.heading(format!("Exuberant Optimizer ({} processes)", process_count));
        ui.heading(format!("CPU: {} | Threads {}", using_cpu, thread_count));

        ui.horizontal(|ui| {
            ui.label("Search:");
            // Connects search_query variable to text box
            ui.text_edit_singleline(&mut self.search_query);
        });

        // Convert HashMap to a Vec so it can be sorted
        let mut process_list: Vec<(&sysinfo::Pid, &sysinfo::Process)> = self.system.processes().iter().collect();

        // Apply search filter first
        if !self.search_query.is_empty() {
            let query = self.search_query.to_lowercase();
            // .retain() keeps only the items that match the condition
            process_list.retain(|(_, p)| {
                p.name().to_string_lossy().to_lowercase().contains(&query)
            });
        }

        process_list.sort_by(|(pid_a, a), (pid_b, b)| {
            let ordering = match self.sort_column {
                SortColumn::Pid =>  pid_a.cmp(pid_b),
                SortColumn::Name => {
                    let name_a = a.name().to_string_lossy().to_lowercase();
                    let name_b = b.name().to_string_lossy().to_lowercase();
                    name_a.cmp(&name_b)
                }
                SortColumn::Cpu => a.cpu_usage().partial_cmp(&b.cpu_usage()).unwrap_or(std::cmp::Ordering::Equal),
                SortColumn::Ram => a.memory().cmp(&b.memory()),
                SortColumn::Disk => {
                    let disk_a = a.disk_usage().read_bytes + a.disk_usage().written_bytes;
                    let disk_b = b.disk_usage().read_bytes + b.disk_usage().written_bytes;
                    disk_a.cmp(&disk_b)
                }
            };

            // Reverse the order if you want descending
            if self.sort_ascending {
                ordering
            } else {
                ordering.reverse()
            }
        });

        // The Process Table
        TableBuilder::new(ui)
            .striped(true) // Makes rows alternate colours
            .vscroll(true)
            .column(Column::initial(60.0)) // PID
            .column(Column::initial(150.0))// Name
            .column(Column::initial(70.0)) // CPU Usage
            .column(Column::initial(80.0)) // RAM Usage
            .column(Column::initial(120.0)) // Disk Usage
            .column(Column::remainder())// Quick Actions
            .header(20.0, |mut header| {
                // Clickable headers to trigger sort
                let mut sort_header = |ui: &mut egui::Ui, label: &str, column: SortColumn| {
                    let is_selected = self.sort_column == column;
                    let text = if is_selected {
                        if self.sort_ascending { format!("{} ⏶", label) } else { format!("{} ⏷", label) }
                    } else {
                        label.to_string()
                    };

                    if ui.selectable_label(is_selected, text).clicked() {
                        if is_selected {
                            // If clicking the same column, flip the order
                            self.sort_ascending = !self.sort_ascending;
                        } else {
                            // If clicking a new column, selected it and default to descending
                            self.sort_column = column;
                            self.sort_ascending = false;
                        }
                    }
                };

                header.col(|ui| sort_header(ui, "PID", SortColumn::Pid));
                header.col(|ui| sort_header(ui, "Name", SortColumn::Name));
                header.col(|ui| sort_header(ui, "CPU", SortColumn::Cpu));
                header.col(|ui| sort_header(ui, "RAM", SortColumn::Ram));
                header.col(|ui| sort_header(ui, "Disk (R / W)", SortColumn::Disk));
                header.col(|ui| { ui.strong("Quick Actions"); });
            })
            .body(|mut body| {
                // Loop through real processes
                for (pid, process) in process_list {
                    let name = process.name().to_string_lossy();
                    // Only show if it matches search
                    body.row(20.0, |mut row| {
                        // PID
                        row.col(|ui| { ui.label(pid.to_string()); });

                        // Name
                        row.col(|ui| { ui.label(name); });

                        // CPU Usage
                        // process.cpu_usage() returns an f32 representing percentage
                        let cpu_usage = process.cpu_usage() / (thread_count as f32);
                        row.col(|ui| { ui.label(format!("{:.1}", cpu_usage)); });

                        // Memory Usage
                        // process.memory() returns bytes
                        let ram_mb = process.memory() / 1024 / 1024;
                        row.col(|ui| { ui.label(format!("{}", ram_mb)); });

                        // Disk Usage
                        // process.disk_usage() returns a struct with read and written bytes
                        let disk = process.disk_usage();
                        let read_kb = disk.read_bytes / 1024;
                        let write_kb = disk.written_bytes / 1024;
                        row.col(|ui| { ui.label(format!("{}K / {}K", read_kb, write_kb)); });

                        // Quick Actions
                        row.col(|ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;

                                if ui.button("SPH").on_hover_text("Set Priority to High").clicked() {
                                    // pid is a sysinfo::Pid, so convert it to u32 for Windows API
                                    boost_process(pid.as_u32());
                                }

                                // Button for Affinity, replace later
                                if ui.button("UPC").on_hover_text("Use Physical Cores").clicked() {
                                    let dynamic_mask = self.get_physical_core_mask();
                                    set_process_affinity(pid.as_u32(), dynamic_mask);
                                }
                            });
                        });
                    });
                }
            });
    }

    fn ui_memory_cleaner(&mut self, ui: &mut egui::Ui) {
        ui.heading("Ram Cleaner and System Timer");
        ui.separator();

        let mut mem_status = MEMORYSTATUSEX::default();
        mem_status.dwLength = size_of::<MEMORYSTATUSEX>() as u32;

        unsafe {
            let _ = GlobalMemoryStatusEx(&mut mem_status);
        }

        // Convert bytes to MB
        let total_mb = mem_status.ullTotalPhys / 1024 / 1024;
        let avail_mb = mem_status.ullAvailPhys / 1024 / 1024;

        // Get standby list size
        let standby_mb = get_standby_list_mb().unwrap_or_else(|e| {
            eprintln!("Error getting standby list: {}", e);
            0
        });

        // Free memory is Windows Available Memory minus the Standby List cache
        let true_free_mb = avail_mb.saturating_sub(standby_mb);

        ui.group(|ui| {
            ui.label(format!("Total MB: {} MB", total_mb));
            ui.label(format!("Standby List: {} MB", standby_mb));
            ui.label(format!("Free MB: {} MB", true_free_mb));
        });

        ui.add_space(10.0);

        // Adjustable Thresholds
        ui.group(|ui| {
            ui.label("Automatic Purge Settings");
            ui.add_space(5.0);

            ui.horizontal(|ui| {
                ui.label("Purge when standby list is at least:");
                ui.add(egui::DragValue::new(&mut self.purge_at_standby_mb)
                    .suffix(" MB")
                    .speed(10.0));
            });

            ui.horizontal(|ui| {
                ui.add_space(20.0);
                ui.label(egui::RichText::new("AND").italics().weak())
            });

            ui.horizontal(|ui| {
                ui.label("Purge when Free Memory is below (MB):");
                ui.add(egui::DragValue::new(&mut self.purge_at_free_mb)
                    .suffix(" MB")
                    .speed(10.0));
            });
        });

        ui.horizontal(|ui| {
           if ui.button("Purge Standby List").on_hover_text("Manually flush the Windows memory cache").clicked() {
               match purge_standby_list() {
                   Ok(_) => self.last_status = format!("Purged at {}", chrono::Local::now().format("%H:%M:%S")),
                   Err(e) => self.last_status = format!("Error: {}", e),
               }
           }
            ui.checkbox(&mut self.is_auto_purge_enabled, "Enable Auto-Purge");
            ui.label(egui::RichText::new(&self.last_status).strong());
        });

        ui.add_space(20.0);
        ui.heading("System Timer Resolution");
        ui.separator();

        // Fetch current timer stats
        match get_timer_resolution() {
            Ok((max_res, min_res, current_res)) => {
                ui.label(format!("Maximum Resolution: {:.4} ms", max_res));
                ui.label(format!("Minimum Resolution: {:.4} ms", min_res));
                ui.label(format!("Current Resolution: {:.4} ms", current_res));
            }
            Err(e) => {
                ui.label(format!("Error fetching timer: {}", e));
            }
        }

        ui.add_space(10.0);

        ui.horizontal(|ui| {
            ui.label("Wanted Timer Resolution:");
            ui.add(egui::DragValue::new(&mut self.timer_res_ms).speed(0.1).range(0.5..=15.6));
            ui.label("ms");
        });

        if ui.button("Enable Custom Timer Resolution").clicked() {
            match set_timer_resolution(self.timer_res_ms) {
                Ok(actual) => self.last_status = format!("Timer set to {:.4} ms", actual),
                Err(e) => self.last_status = format!("Timer Error: {}", e),
            }
        }
    }
}

// What to draw (the ui)
impl eframe::App for OptimizerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let now = std::time::Instant::now();

        // Tell the system to look for changes
        if now.duration_since(self.last_refresh).as_secs() >= 2 {
            if matches!(self.active_tab, ActiveTab::Processes) {
                self.system.refresh_processes(ProcessesToUpdate::All, true);
                self.system.refresh_cpu_all();
            } else if matches!(self.active_tab, ActiveTab::MemoryCleaner) {
                self.system.refresh_memory();
            }
            self.last_refresh = now;
        }

        if self.is_auto_purge_enabled {
            let mut mem_status = MEMORYSTATUSEX::default();
            mem_status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;

            unsafe {
                let _ = GlobalMemoryStatusEx(&mut mem_status);
            }

            let avail_mb = mem_status.ullAvailPhys / 1024 / 1024;
            let standby_mb = get_standby_list_mb().unwrap_or(0);
            let true_free_mb = avail_mb.saturating_sub(standby_mb);

            // Check if both thresholds are met
            if standby_mb >= self.purge_at_standby_mb && true_free_mb <= self.purge_at_free_mb {
                match purge_standby_list() {
                    Ok(_) => self.last_status = "Auto-purged successfully.".to_string(),
                    Err(e) => self.last_status = format!("Auto-purge Error: {}", e),
                }
            }
        }

        egui::SidePanel::left("nav_panel").show(ctx, |ui| {
           ui.heading("Navigation");
            ui.separator();

            if ui.selectable_label(matches!(self.active_tab, ActiveTab::Processes), "Processes").clicked() {
                self.active_tab = ActiveTab::Processes;
            }
            if ui.selectable_label(matches!(self.active_tab, ActiveTab::MemoryCleaner), "RAM Cleaner").clicked() {
                self.active_tab = ActiveTab::MemoryCleaner;
            }
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            match self.active_tab {
                ActiveTab::Processes => self.ui_processes(ui),
                ActiveTab::MemoryCleaner => self.ui_memory_cleaner(ui),
            }
        });

        // Increased refresh to 2 seconds - make it so user can set
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions::default();

    // Handles complicated Windows stuff :)
    eframe::run_native(
        "Exuberant Optimizer",
        options,
        Box::new(|_cc| Ok(Box::new(OptimizerApp::default()))), // Since window size may change, put in box (heap memory)
    )
}

fn boost_process(pid: u32) {
    unsafe {
        // Open a handle to the process with permission to change its info
        let handle_result = OpenProcess(
            PROCESS_SET_INFORMATION,
            false,
            pid
        );

        if let Ok(handle) = handle_result {
            // Tell Windows to set process priority to "High"
            let _ = SetPriorityClass(handle, HIGH_PRIORITY_CLASS);

            // Close handle to avoid memory leak
            let _ = CloseHandle(handle);

            println!("Process set high priority class to {}", pid);
        } else {
            eprintln!("Failed to open PID: {}. Trying running as Admin.", pid);
        }
    }
}

fn set_process_affinity(pid: u32, mask: usize) {
    unsafe {
        let handle_result = OpenProcess(
            PROCESS_SET_INFORMATION,
            false,
            pid
        );

        if let Ok(handle) = handle_result {
            // SetProcessAffinityMask takes the handle and the bitmask
            let _ = SetProcessAffinityMask(handle, mask);
            let _ = CloseHandle(handle);
            println!("Set affinity mask {} for PID: {}", mask, pid);
        }
    }
}

fn enable_privilege(privilege_name: windows::core::PCWSTR) -> windows::core::Result<()> {
    unsafe {
        let mut token = HANDLE::default();

        // Open the current process token
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )?;

        // Lookup LUID for the privilege
        let mut luid = Default::default();
        LookupPrivilegeValueW(None, privilege_name, &mut luid)?;

        // Prepare the token privileges struct
        let mut tp = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };

        // Adjust the token
        AdjustTokenPrivileges(token, false, Some(&mut tp), 0, None, None)?;

        CloseHandle(token)?;
    }
    Ok(())
}
pub fn purge_standby_list() -> Result<(), String> {
    // Call NtSetSystemInformation
    // Class 80 is SystemMemoryListInformation
    // Command 4 is MemoryPurgeStandbyList
    let command: i32 = 4;
    let status = unsafe {
        NtSetSystemInformation(
            80,
            &command as *const _ as *const std::ffi::c_void,
            std::mem::size_of::<i32>() as u32,
        )
    };

    // NTSTATUS >= 0 means success
    if status >= 0 {
        Ok(())
    } else {
        Err(format!("NtSetSystemInformation failed with NTSTATUS code : {:#X}", status))
    }
}

pub fn get_timer_resolution() -> Result<(f32, f32, f32), String> {
    let mut min: u32 = 0;
    let mut max: u32 = 0;
    let mut current: u32 = 0;

    let status = unsafe { NtQueryTimerResolution(&mut min, &mut max, &mut current) };

    if status >= 0 {
        // Convert from 100-ns intervals to milliseconds
        Ok((
            max as f32 / 10000.0,
            min as f32 / 10000.0,
            current as f32 / 10000.0,
            ))
    } else {
        Err(format!("NtQueryTimerResolution failed with status: {:#X}", status))
    }
}

pub fn set_timer_resolution(ms: f32) -> Result<f32, String> {
    let desired_time = (ms * 10000.0) as u32;
    let mut actual_time: u32 = 0;

    // 1 means "Set", 0 means "Unset/Revert"
    let status = unsafe { NtSetTimerResolution(desired_time, 1, &mut actual_time) };

    if status >= 0 {
        Ok(actual_time as f32 / 10000.0)
    } else {
        Err(format!("NtSetTimerResolution failed: {:#X}", status))
    }
}

pub fn get_standby_list_mb() -> Result<u64, String> {
    let mut mem_list_info = SystemMemoryListInformation::default();
    let mut return_length: u32 = 0;

    let status = unsafe {
        NtQuerySystemInformation(
            80, // SystemMemoryListInformation
            &mut mem_list_info as *mut _ as *mut std::ffi::c_void,
            size_of::<SystemMemoryListInformation>() as u32,
            &mut return_length,
        )
    };

    if status >= 0 {
        // The Standby List is the sum of all 8 priority page lists
        let standby_pages: usize = mem_list_info.page_count_by_priority.iter().sum();

        // 1 page = 4096 bytes (4KB)
        let standby_bytes = standby_pages as u64 * 4096;

        // Convert to MB
        Ok(standby_bytes / 1024 / 1024)
    } else {
        Err(format!("NtQuerySystemInformation failed: {:#X}", status))
    }
}
