// Copyright 2023 Datafuse Labs.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use chrono::TimeZone;
use chrono::Utc;
use databend_common_meta_app::schema::CatalogOption;
use databend_common_meta_app::schema::IcebergCatalogOption;
use databend_common_meta_app::schema::IcebergGlueCatalogOption;
use fastrace::func_name;
use std::collections::HashMap;

use crate::common;

// These bytes are built when a new version in introduced,
// and are kept for backward compatibility test.
//
// *************************************************************
// * These messages should never be updated,                   *
// * only be added when a new version is added,                *
// * or be removed when an old version is no longer supported. *
// *************************************************************
//
// The message bytes are built from the output of `test_pb_from_to()`
#[test]
fn test_v111_add_glue_as_iceberg_catalog_option() -> anyhow::Result<()> {
    let catalog_meta_v111 = vec![
        18, 55, 26, 53, 18, 45, 10, 21, 104, 116, 116, 112, 58, 47, 47, 49, 50, 55, 46, 48, 46, 48,
        46, 49, 58, 57, 57, 48, 48, 18, 14, 115, 51, 58, 47, 47, 109, 121, 95, 98, 117, 99, 107,
        101, 116, 160, 6, 98, 168, 6, 24, 160, 6, 98, 168, 6, 24, 162, 1, 23, 50, 48, 49, 52, 45,
        49, 49, 45, 50, 56, 32, 49, 50, 58, 48, 48, 58, 48, 57, 32, 85, 84, 67, 160, 6, 98, 168, 6,
        24,
    ];

    let mut props = HashMap::new();
    props.insert("AWS_KEY_ID".to_string(), "super secure access key".to_string());
    props.insert("AWS_SECRET_KEY".to_string(), "even more secure secret key".to_string());
    props.insert("REGION".to_string(), "us-east-1 aka anti-multi-availability".to_string());

    let want = || databend_common_meta_app::schema::CatalogMeta {
        catalog_option: CatalogOption::Iceberg(IcebergGlueCatalogOption::Rest(
            IcebergGlueCatalogOption {
                address: "http://127.0.0.1:9900".to_string(),
                warehouse: "s3://my_bucket".to_string(),
                props,
            },
        )),
        created_on: Utc.with_ymd_and_hms(2014, 11, 28, 12, 0, 9).unwrap(),
    };

    common::test_pb_from_to(func_name!(), want())?;
    common::test_load_old(func_name!(), catalog_meta_v111.as_slice(), 111, want())?;

    Ok(())
}