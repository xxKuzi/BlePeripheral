use std::io::{self, BufRead};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::sync::{mpsc, Mutex};
use uuid::Uuid;

use ble_peripheral_rust::{
    gatt::{
        characteristic::Characteristic,
        descriptor::Descriptor,
        peripheral_event::{
            PeripheralEvent, ReadRequestResponse, RequestResponse, WriteRequestResponse,
        },
        properties::{AttributePermission, CharacteristicProperty},
        service::Service,
    },
    uuid::ShortUuid,
    Peripheral, PeripheralImpl,
};

static STATE: AtomicBool = AtomicBool::new(false);

#[tokio::main]
async fn main() {
    std::env::set_var("RUST_LOG", "info");
    if let Err(err) = pretty_env_logger::try_init() {
        eprintln!("WARNING: failed to initialize logging framework: {}", err);
    }
    start_app().await;
}

async fn start_app() {
    let char_uuid = Uuid::from_short(0x2A3D_u16);

    // Define a service with characteristics.
    let service = Service {
        uuid: Uuid::from_short(0x1234_u16),
        primary: true,
        characteristics: vec![
            Characteristic {
                uuid: char_uuid,
                properties: vec![
                    CharacteristicProperty::Read,
                    CharacteristicProperty::Write,
                    CharacteristicProperty::Notify,
                ],
                permissions: vec![
                    AttributePermission::Readable,
                    AttributePermission::Writeable,
                ],
                value: None,
                descriptors: vec![Descriptor {
                    uuid: Uuid::from_short(0x2A13_u16),
                    value: Some(vec![0, 1]),
                    ..Default::default()
                }],
            },
            Characteristic {
                uuid: Uuid::from_string("1209"),
                ..Default::default()
            },
        ],
    };

    let (sender_tx, mut receiver_rx) = mpsc::channel::<PeripheralEvent>(256);

    // Create the peripheral and wrap it in an Arc with a Mutex.
    let peripheral = Arc::new(Mutex::new(
        Peripheral::new(sender_tx).await.unwrap(),
    ));

    // Clone the peripheral and char_uuid for the event handler.
    let peripheral_for_events = peripheral.clone();
    let char_uuid_for_events = char_uuid.clone();
    tokio::spawn(async move {
        while let Some(event) = receiver_rx.recv().await {
            handle_updates(event, peripheral_for_events.clone(), char_uuid_for_events).await;
        }
    });

    // Wait until the peripheral is powered on.
    loop {
        let powered = {
            let mut periph = peripheral.lock().await;
            periph.is_powered().await.unwrap_or(false)
        };
        if powered {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    // Add the service.
    {
        let mut periph = peripheral.lock().await;
        if let Err(err) = periph.add_service(&service).await {
            log::error!("Error adding service: {}", err);
            return;
        }
    }
    log::info!("Service Added");

    // Start advertising.
    {
        let mut periph = peripheral.lock().await;
        if let Err(err) = periph.start_advertising("RustBLE", &[service.uuid]).await {
            log::error!("Error starting advertising: {}", err);
            return;
        }
    }
    log::info!("Advertising Started");

    // Read from stdin to update the characteristic manually.
    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        match line {
            Ok(input) => {
                let trimmed_input = input.trim().to_lowercase();
                match trimmed_input.as_str() {
                    "on" => {
                        STATE.store(true, Ordering::SeqCst);
                        println!("STATE changed to: ON ✅");
                    }
                    "off" => {
                        STATE.store(false, Ordering::SeqCst);
                        println!("STATE changed to: OFF ❌");
                    }
                    _ => {
                        println!("Writing: {} to {:?}", input, char_uuid);
                    }
                }
                // Update the characteristic to notify subscribed clients.
                let mut periph = peripheral.lock().await;
                if let Err(e) = periph.update_characteristic(char_uuid, input.into()).await {
                    log::error!("Error updating characteristic: {:?}", e);
                }
            }
            Err(err) => {
                log::error!("Error reading from console: {}", err);
                break;
            }
        }
    }
}

async fn handle_updates(
    event: PeripheralEvent,
    peripheral: Arc<Mutex<Peripheral>>,
    char_uuid: Uuid,
) {
    match event {
        PeripheralEvent::StateUpdate { is_powered } => {
            log::info!("PowerOn: {:?}", is_powered);
        }
        PeripheralEvent::CharacteristicSubscriptionUpdate { request, subscribed } => {
            log::info!(
                "CharacteristicSubscriptionUpdate: Subscribed {} {:?}",
                subscribed,
                request
            );
        }
        PeripheralEvent::ReadRequest {
            request,
            offset,
            responder,
        } => {
            let current_state = STATE.load(Ordering::SeqCst);
            let response_value = if current_state { "on" } else { "off" };

            log::info!(
                "ReadRequest: {:?} Offset: {} -> Responding: {}",
                request,
                offset,
                response_value
            );

            if let Err(e) = responder.send(ReadRequestResponse {
                value: response_value.into(),
                response: RequestResponse::Success,
            }) {
                log::error!("Failed to send read response: {:?}", e);
            }
        }
        PeripheralEvent::WriteRequest {
            request,
            offset,
            value,
            responder,
        } => {
            if let Ok(msg) = String::from_utf8(value.clone()) {
                log::info!("WriteRequest: Received message -> {}", msg);

                let new_value = match msg.trim() {
                    "on" => {
                        STATE.store(true, Ordering::SeqCst);
                        log::info!("STATE changed to: ON ✅");
                        "on"
                    }
                    "off" => {
                        STATE.store(false, Ordering::SeqCst);
                        log::info!("STATE changed to: OFF ❌");
                        "off"
                    }
                    _ => {
                        log::warn!("WriteRequest: Unrecognized value -> {}", msg);
                        msg.as_str()
                    }
                };

                // Update the characteristic to notify subscribed clients.
                if let Err(e) = peripheral
                    .lock()
                    .await
                    .update_characteristic(char_uuid, new_value.into())
                    .await
                {
                    log::error!("Error updating characteristic in WriteRequest: {:?}", e);
                }
            } else {
                log::error!("WriteRequest: Received non-UTF8 data");
            }

            if let Err(e) = responder.send(WriteRequestResponse {
                response: RequestResponse::Success,
            }) {
                log::error!("Failed to send write response: {:?}", e);
            }
        }
        _ => {
            log::info!("Unhandled event: {:?}", event);
        }
    }
}
