//
// Copyright (c) 2022 ZettaScale Technology
//
// This program and the accompanying materials are made available under the
// terms of the Eclipse Public License 2.0 which is available at
// http://www.eclipse.org/legal/epl-2.0, or the Apache License, Version 2.0
// which is available at https://www.apache.org/licenses/LICENSE-2.0.
//
// SPDX-License-Identifier: EPL-2.0 OR Apache-2.0
//
// Contributors:
//   ZettaScale Zenoh Team, <zenoh@zettascale.tech>
//
use crate::config::Config;
use crate::mqtt_helpers::*;
use async_std::sync::RwLock;
use lazy_static::__Deref;
use ntex::util::{ByteString, Bytes};
use std::convert::TryInto;
use std::{collections::HashMap, sync::Arc};
use zenoh::plugins::ZResult;
use zenoh::prelude::r#async::*;
use zenoh::subscriber::Subscriber;

#[derive(Debug)]
pub(crate) struct MqttSessionState<'a> {
    pub(crate) client_id: String,
    pub(crate) zsession: Arc<Session>,
    pub(crate) config: Arc<Config>,
    pub(crate) subs: RwLock<HashMap<String, Subscriber<'a, ()>>>,
}

impl MqttSessionState<'_> {
    pub(crate) fn new<'a>(
        client_id: String,
        zsession: Arc<Session>,
        config: Arc<Config>,
    ) -> MqttSessionState<'a> {
        MqttSessionState {
            client_id,
            zsession,
            config,
            subs: RwLock::new(HashMap::new()),
        }
    }

    pub(crate) async fn map_mqtt_subscription<'a>(
        &'a self,
        topic: &str,
        sink: MqttSink,
    ) -> ZResult<()> {
        if is_allowed(topic, &self.config) {
            let mut subs = self.subs.write().await;
            if !subs.contains_key(topic) {
                let ke = mqtt_topic_to_ke(topic, &self.config.scope)?;
                let client_id = self.client_id.clone();
                let config = self.config.clone();
                let sub = self
                    .zsession
                    .declare_subscriber(ke)
                    .callback(move |sample| {
                        if let Err(e) = route_zenoh_to_mqtt(sample, &client_id, &config, &sink) {
                            log::warn!("{}", e);
                        }
                    })
                    .res()
                    .await?;
                subs.insert(topic.into(), sub);
                Ok(())
            } else {
                log::debug!(
                    "MQTT Client {} already subscribes to {} => ignore",
                    self.client_id,
                    topic
                );
                Ok(())
            }
        } else {
            log::info!(
                "MQTT Client {}: ignoring its subscription to '{}' topic - not allowed (see your 'allow' or 'deny' configuration)",
                self.client_id,
                topic
            );
            Ok(())
        }
    }

    pub(crate) async fn route_mqtt_to_zenoh(
        &self,
        mqtt_topic: &ntex::router::Path<ByteString>,
        payload: &Bytes,
    ) -> ZResult<()> {
        let topic = mqtt_topic.get_ref().as_str();
        if is_allowed(topic, &self.config) {
            let ke: KeyExpr = if let Some(scope) = &self.config.scope {
                (scope / topic.try_into()?).into()
            } else {
                topic.try_into()?
            };
            let encoding = guess_encoding(payload.deref());
            // TODO: check allow/deny
            log::trace!(
                "MQTT client {}: route from MQTT '{}' to Zenoh '{}' (encoding={})",
                self.client_id,
                topic,
                ke,
                encoding
            );
            self.zsession
                .put(ke, payload.deref())
                .encoding(encoding)
                .res()
                .await
        } else {
            log::info!(
                "MQTT Client {}: ignoring its publication to '{}' topic - not allowed (see your 'allow' or 'deny' configuration)",
                self.client_id,
                topic
            );
            Ok(())
        }
    }
}

fn route_zenoh_to_mqtt(
    sample: Sample,
    client_id: &str,
    config: &Config,
    sink: &MqttSink,
) -> ZResult<()> {
    let topic = ke_to_mqtt_topic_publish(&sample.key_expr, &config.scope)?;
    log::trace!(
        "MQTT client {}: route from Zenoh '{}' to MQTT '{}'",
        client_id,
        sample.key_expr,
        topic
    );
    sink.publish_at_most_once(topic, sample.payload.contiguous().to_vec().into())
        .map_err(|e| {
            zerror!(
                "MQTT client {}: error re-publishing on MQTT a Zenoh publication on {}: {}",
                client_id,
                sample.key_expr,
                e
            )
            .into()
        })
}
