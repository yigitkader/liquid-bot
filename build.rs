// build.rs - Solend layout codegen per Structure.md section 11.5
// Reads layout JSON files from idl/ directory and generates Rust structs

use std::env;
use std::fs;
use std::path::PathBuf;
use std::collections::HashMap;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=idl/solend_last_update_layout.json");
    println!("cargo:rerun-if-changed=idl/solend_lending_market_layout.json");
    println!("cargo:rerun-if-changed=idl/solend_reserve_layout.json");
    println!("cargo:rerun-if-changed=idl/solend_obligation_layout.json");
    
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    
    // Per Structure.md section 11.5: Read layout JSON files from idl/
    // build.rs does NOT download from internet - it reads from pre-generated JSON files
    let layout_files = vec![
        "idl/solend_last_update_layout.json",
        "idl/solend_lending_market_layout.json",
        "idl/solend_reserve_layout.json",
        "idl/solend_obligation_layout.json",
    ];
    
    // Check if layout files exist, otherwise fall back to Anchor IDL format
    let use_layout_format = layout_files.iter().all(|f| PathBuf::from(f).exists());
    
    let mut all_types: HashMap<String, LayoutType> = HashMap::new();
    let mut all_accounts: HashMap<String, LayoutAccount> = HashMap::new();
    let mut sdk_version = "unknown".to_string();
    
    if use_layout_format {
        // Read layout JSON format per Structure.md section 11.3
        for layout_file in &layout_files {
            let content = fs::read_to_string(layout_file)
                .unwrap_or_else(|_| panic!("Failed to read layout file: {}", layout_file));
            
            let layout: serde_json::Value = serde_json::from_str(&content)
                .unwrap_or_else(|_| panic!("Failed to parse layout JSON: {}", layout_file));
            
            // Extract metadata
            if let Some(meta) = layout.get("meta") {
                if let Some(version) = meta.get("sdkVersion").and_then(|v| v.as_str()) {
                    sdk_version = version.to_string();
                }
            }
            
            // Process types
            if let Some(types) = layout.get("types").and_then(|t| t.as_array()) {
                for ty in types {
                    if let Some(name) = ty.get("name").and_then(|n| n.as_str()) {
                        if let Some(fields) = ty.get("fields").and_then(|f| f.as_array()) {
                            all_types.insert(name.to_string(), LayoutType {
                                name: name.to_string(),
                                fields: fields.clone(),
                            });
                        }
                    }
                }
            }
            
            // Process accounts
            if let Some(accounts) = layout.get("accounts").and_then(|a| a.as_array()) {
                for acc in accounts {
                    if let Some(name) = acc.get("name").and_then(|n| n.as_str()) {
                        if let Some(fields) = acc.get("fields").and_then(|f| f.as_array()) {
                            all_accounts.insert(name.to_string(), LayoutAccount {
                                name: name.to_string(),
                                fields: fields.clone(),
                            });
                        }
                    }
                }
            }
        }
    } else {
        // Fallback: Parse Anchor IDL format (backward compatibility)
        let idl_path = PathBuf::from("idl/solend.json");
        if idl_path.exists() {
            let idl_content = fs::read_to_string(&idl_path)
                .expect("Failed to read idl/solend.json");
            
            let idl: serde_json::Value = serde_json::from_str(&idl_content)
                .expect("Failed to parse IDL JSON");
            
            sdk_version = idl.get("version").and_then(|v| v.as_str())
                .unwrap_or("unknown").to_string();
            
            // Process types section
            if let Some(types) = idl.get("types").and_then(|t| t.as_array()) {
                for ty in types {
                    if let Some(name) = ty.get("name").and_then(|n| n.as_str()) {
                        if let Some(ty_def) = ty.get("type") {
                            if let Some(fields) = ty_def.get("fields").and_then(|f| f.as_array()) {
                                all_types.insert(name.to_string(), LayoutType {
                                    name: name.to_string(),
                                    fields: fields.clone(),
                                });
                            }
                        }
                    }
                }
            }
            
            // Process accounts section
            if let Some(accounts) = idl.get("accounts").and_then(|a| a.as_array()) {
                for account in accounts {
                    if let Some(name) = account.get("name").and_then(|n| n.as_str()) {
                        if let Some(ty) = account.get("type") {
                            if let Some(fields) = ty.get("fields").and_then(|f| f.as_array()) {
                                all_accounts.insert(name.to_string(), LayoutAccount {
                                    name: name.to_string(),
                                    fields: fields.clone(),
                                });
                            }
                        }
                    }
                }
            }
        } else {
            panic!(
                "No layout files found!\n\
                Expected layout JSON files (per Structure.md section 11.5):\n\
                - idl/solend_last_update_layout.json\n\
                - idl/solend_lending_market_layout.json\n\
                - idl/solend_reserve_layout.json\n\
                - idl/solend_obligation_layout.json\n\
                \n\
                Or fallback Anchor IDL:\n\
                - idl/solend.json\n\
                \n\
                Please run: cd tools/solend-layout-dump && npm install && npm run dump-layouts"
            );
        }
    }
    
    // Generate Rust code (no imports - they're in solend.rs)
    let mut rust_code = String::from(
        "// Auto-generated Solend account layouts from IDL\n\
        // DO NOT EDIT MANUALLY - Generated by build.rs per Structure.md section 11.5\n\n"
    );
    
    // Generate all types first
    for (name, layout_type) in &all_types {
        rust_code.push_str(&generate_struct_from_layout(name, &layout_type.fields, &all_types, &all_accounts));
        rust_code.push('\n');
    }
    
    // Generate all accounts
    for (name, layout_account) in &all_accounts {
        // Skip if already generated as a type
        if !all_types.contains_key(name) {
            rust_code.push_str(&generate_struct_from_layout(name, &layout_account.fields, &all_types, &all_accounts));
            rust_code.push('\n');
        }
    }
    
    // Write generated code
    let output_path = out_dir.join("solend_layout.rs");
    fs::write(&output_path, rust_code)
        .expect("Failed to write solend_layout.rs");
    
    println!("cargo:warning=Generated Solend layouts at {:?} (SDK version: {})", output_path, sdk_version);
}

struct LayoutType {
    name: String,
    fields: Vec<serde_json::Value>,
}

struct LayoutAccount {
    name: String,
    fields: Vec<serde_json::Value>,
}


fn calculate_hash(content: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    format!("{:x}", hasher.finish())
}


fn generate_struct_from_layout(
    name: &str,
    fields: &[serde_json::Value],
    all_types: &HashMap<String, LayoutType>,
    all_accounts: &HashMap<String, LayoutAccount>,
) -> String {
    let mut code = format!("#[derive(BorshDeserialize, BorshSerialize, Debug, Clone)]\npub struct {} {{\n", name);
    
    for field in fields {
        if let Some(field_name) = field.get("name").and_then(|n| n.as_str()) {
            let rust_type = layout_field_to_rust(field, all_types, all_accounts);
            code.push_str(&format!("    pub {}: {},\n", field_name, rust_type));
        }
    }
    
    code.push_str("}\n");
    code
}

fn layout_field_to_rust(
    field: &serde_json::Value,
    all_types: &HashMap<String, LayoutType>,
    all_accounts: &HashMap<String, LayoutAccount>,
) -> String {
    // Per Structure.md section 11.3: Field format is { kind: "scalar"|"array"|"custom", name, type, ... }
    if let Some(kind) = field.get("kind").and_then(|k| k.as_str()) {
        match kind {
            "scalar" => {
                if let Some(ty) = field.get("type").and_then(|t| t.as_str()) {
                    match ty {
                        "u8" => "u8".to_string(),
                        "u64" => "u64".to_string(),
                        "u128" => "u128".to_string(),
                        "bool" => "bool".to_string(),
                        "Pubkey" => "Pubkey".to_string(),
                        "publicKey" => "Pubkey".to_string(),
                        _ => ty.to_string(),
                    }
                } else {
                    "u8".to_string()
                }
            }
            "array" => {
                if let Some(element_type) = field.get("elementType").and_then(|t| t.as_str()) {
                    if let Some(len) = field.get("len").and_then(|l| l.as_u64()) {
                        format!("[{}; {}]", element_type, len)
                    } else {
                        format!("Vec<{}>", element_type)
                    }
                } else {
                    "Vec<u8>".to_string()
                }
            }
            "custom" => {
                if let Some(ty) = field.get("type").and_then(|t| t.as_str()) {
                    ty.to_string()
                } else {
                    "u8".to_string()
                }
            }
            _ => "u8".to_string(),
        }
    } else {
        // Fallback: Try to parse as Anchor IDL format
        if let Some(field_type) = field.get("type") {
            idl_type_to_rust_legacy(field_type, all_types, all_accounts)
        } else {
            "u8".to_string()
        }
    }
}

fn idl_type_to_rust_legacy(
    ty: &serde_json::Value,
    all_types: &HashMap<String, LayoutType>,
    all_accounts: &HashMap<String, LayoutAccount>,
) -> String {
    match ty {
        serde_json::Value::String(s) => {
            match s.as_str() {
                "u8" => "u8".to_string(),
                "u64" => "u64".to_string(),
                "u128" => "u128".to_string(),
                "bool" => "bool".to_string(),
                "publicKey" => "Pubkey".to_string(),
                "bytes" => "Vec<u8>".to_string(),
                "string" => "String".to_string(),
                _ => {
                    if all_types.contains_key(s) || all_accounts.contains_key(s) {
                        s.to_string()
                    } else {
                        format!("{}", s)
                    }
                }
            }
        }
        serde_json::Value::Object(obj) => {
            if let Some(name) = obj.get("defined").and_then(|d| d.as_str()) {
                return name.to_string();
            }
            
            if let Some(kind) = obj.get("kind").and_then(|k| k.as_str()) {
                match kind {
                    "vec" => {
                        if let Some(inner) = obj.get("vec") {
                            let inner_type = idl_type_to_rust_legacy(inner, all_types, all_accounts);
                            format!("Vec<{}>", inner_type)
                        } else {
                            "Vec<u8>".to_string()
                        }
                    }
                    "option" => {
                        if let Some(inner) = obj.get("option") {
                            let inner_type = idl_type_to_rust_legacy(inner, all_types, all_accounts);
                            format!("Option<{}>", inner_type)
                        } else {
                            "Option<u8>".to_string()
                        }
                    }
                    "defined" => {
                        if let Some(name) = obj.get("defined").and_then(|d| d.as_str()) {
                            name.to_string()
                        } else {
                            "u8".to_string()
                        }
                    }
                    "array" => {
                        if let Some(size) = obj.get("size").and_then(|s| s.as_u64()) {
                            if let Some(ty) = obj.get("type") {
                                let inner_type = idl_type_to_rust_legacy(ty, all_types, all_accounts);
                                format!("[{}; {}]", inner_type, size)
                            } else {
                                format!("[u8; {}]", size)
                            }
                        } else {
                            "[u8; 1]".to_string()
                        }
                    }
                    _ => "u8".to_string(),
                }
            } else if obj.contains_key("vec") {
                if let Some(inner) = obj.get("vec") {
                    let inner_type = idl_type_to_rust_legacy(inner, all_types, all_accounts);
                    format!("Vec<{}>", inner_type)
                } else {
                    "Vec<u8>".to_string()
                }
            } else {
                "u8".to_string()
            }
        }
        _ => "u8".to_string(),
    }
}

