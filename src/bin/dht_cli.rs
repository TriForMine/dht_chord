use std::io::{self, Read, Write};
use std::net::TcpStream;

const API_DHT_PUT: u16 = 650;
const API_DHT_GET: u16 = 651;
const API_DHT_SUCCESS: u16 = 652;
const API_DHT_FAILURE: u16 = 653;
const API_DHT_SHUTDOWN: u16 = 654;

struct DHTClient {
    host: String,
    port: u16,
}

impl DHTClient {
    fn new(host: &str, port: u16) -> Self {
        Self {
            host: host.to_string(),
            port,
        }
    }

    fn hash_key_bytes(key: &str) -> [u8; 32] {
        // Use SHA-256 to create a 32-byte key, matching the protocol
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(key.as_bytes());
        let result = hasher.finalize();
        result.into()
    }

    fn put_value(
        &self,
        key: &str,
        value: &str,
        ttl: u16,
        replication: u8,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut stream = TcpStream::connect(format!("{}:{}", self.host, self.port))?;

        let key_bytes = Self::hash_key_bytes(key);
        let value_bytes = value.as_bytes();

        // Calculate total message size: header(4) + ttl(2) + replication(1) + reserved(1) + key(32) + value
        let total_size = 4u16 + 2 + 1 + 1 + 32 + value_bytes.len() as u16;

        // Write header: size(2) + message_type(2)
        stream.write_all(&total_size.to_be_bytes())?;
        stream.write_all(&API_DHT_PUT.to_be_bytes())?;

        // Write payload: ttl(2) + replication(1) + reserved(1) + key(32) + value
        stream.write_all(&ttl.to_be_bytes())?;
        stream.write_all(&[replication])?;
        stream.write_all(&[0u8])?; // reserved byte
        stream.write_all(&key_bytes)?;
        stream.write_all(value_bytes)?;

        println!(
            "✓ PUT '{}' = '{}' (TTL: {}s, Replication: {})",
            key, value, ttl, replication
        );
        Ok(())
    }

    fn get_value(&self, key: &str) -> Result<Option<String>, Box<dyn std::error::Error>> {
        let mut stream = TcpStream::connect(format!("{}:{}", self.host, self.port))?;

        let key_bytes = Self::hash_key_bytes(key);

        // Calculate total message size: header(4) + key(32)
        let total_size = 4u16 + 32;

        // Write header: size(2) + message_type(2)
        stream.write_all(&total_size.to_be_bytes())?;
        stream.write_all(&API_DHT_GET.to_be_bytes())?;

        // Write payload: key(32)
        stream.write_all(&key_bytes)?;

        // Read response header
        let mut header = [0u8; 4];
        stream.read_exact(&mut header)?;

        let response_size = u16::from_be_bytes([header[0], header[1]]);
        let response_type = u16::from_be_bytes([header[2], header[3]]);

        // Read response payload
        let payload_size = response_size - 4;
        let mut payload = vec![0u8; payload_size as usize];
        stream.read_exact(&mut payload)?;

        match response_type {
            API_DHT_SUCCESS => {
                // Payload: key(32) + value
                let value = String::from_utf8(payload[32..].to_vec())?;
                println!("✓ GET '{}' = '{}'", key, value);
                Ok(Some(value))
            }
            API_DHT_FAILURE => {
                println!("✗ GET '{}' - not found", key);
                Ok(None)
            }
            _ => Err(format!("Unexpected response type: {}", response_type).into()),
        }
    }

    fn shutdown(&self) -> Result<(), Box<dyn std::error::Error>> {
        let mut stream = TcpStream::connect(format!("{}:{}", self.host, self.port))?;

        // Shutdown message only has header
        let total_size = 4u16;

        // Write header: size(2) + message_type(2)
        stream.write_all(&total_size.to_be_bytes())?;
        stream.write_all(&API_DHT_SHUTDOWN.to_be_bytes())?;

        println!("✓ Shutdown request sent");
        Ok(())
    }
}

fn print_help() {
    println!("DHT Chord CLI Client");
    println!("Commands:");
    println!("  GET <key>                           - Retrieve value for key");
    println!("  PUT <key> <value>                   - Store key-value (TTL: 60s, Replication: 1)");
    println!("  PUT <key> <value> <ttl>             - Store with custom TTL");
    println!("  PUT <key> <value> <ttl> <replication> - Store with TTL and replication");
    println!("  SHUTDOWN                            - Shutdown the DHT node");
    println!("  HELP                                - Show this help");
    println!("  QUIT or EXIT                        - Exit this client");
    println!();
    println!("Examples:");
    println!("  PUT hello world");
    println!("  PUT hello world 120");
    println!("  PUT hello world 120 3");
    println!("  GET hello");
}

fn parse_command(input: &str) -> Vec<String> {
    input
        .trim()
        .split_whitespace()
        .map(|s| s.to_string())
        .collect()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();

    let (host, port) = match args.len() {
        1 => ("127.0.0.1".to_string(), 7401u16),
        2 => {
            // Parse host:port format
            if let Some(colon_pos) = args[1].find(':') {
                let host = args[1][..colon_pos].to_string();
                let port = args[1][colon_pos + 1..].parse::<u16>()?;
                (host, port)
            } else {
                (args[1].clone(), 7401u16)
            }
        }
        3 => (args[1].clone(), args[2].parse::<u16>()?),
        _ => {
            eprintln!("Usage: {} [host] [port] or [host:port]", args[0]);
            eprintln!("Default: 127.0.0.1:7401");
            std::process::exit(1);
        }
    };

    let client = DHTClient::new(&host, port);

    println!("DHT Chord CLI Client");
    println!("Connected to {}:{}", host, port);
    println!("Type 'HELP' for commands or 'QUIT' to exit");
    println!();

    loop {
        print!("dht> ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;

        let parts = parse_command(&input);
        if parts.is_empty() {
            continue;
        }

        match parts[0].to_uppercase().as_str() {
            "QUIT" | "EXIT" => {
                println!("Goodbye!");
                break;
            }
            "HELP" => {
                print_help();
            }
            "GET" => {
                if parts.len() != 2 {
                    println!("Usage: GET <key>");
                    continue;
                }

                if let Err(e) = client.get_value(&parts[1]) {
                    eprintln!("Error: {}", e);
                }
            }
            "PUT" => {
                let (key, value, ttl, replication) = match parts.len() {
                    3 => (&parts[1], &parts[2], 60u16, 1u8),
                    4 => match parts[3].parse::<u16>() {
                        Ok(ttl) => (&parts[1], &parts[2], ttl, 1u8),
                        Err(_) => {
                            println!("Error: TTL must be a number");
                            continue;
                        }
                    },
                    5 => {
                        let ttl = match parts[3].parse::<u16>() {
                            Ok(ttl) => ttl,
                            Err(_) => {
                                println!("Error: TTL must be a number");
                                continue;
                            }
                        };
                        let replication = match parts[4].parse::<u8>() {
                            Ok(r) if r > 0 => r,
                            _ => {
                                println!("Error: Replication must be a positive number (1-255)");
                                continue;
                            }
                        };
                        (&parts[1], &parts[2], ttl, replication)
                    }
                    _ => {
                        println!("Usage: PUT <key> <value> [ttl] [replication]");
                        continue;
                    }
                };

                if let Err(e) = client.put_value(key, value, ttl, replication) {
                    eprintln!("Error: {}", e);
                }
            }
            "SHUTDOWN" => {
                if let Err(e) = client.shutdown() {
                    eprintln!("Error: {}", e);
                } else {
                    println!("Node shutdown request sent. Exiting client.");
                    break;
                }
            }
            _ => {
                println!(
                    "Unknown command: '{}'. Type 'HELP' for available commands.",
                    parts[0]
                );
            }
        }
    }

    Ok(())
}
