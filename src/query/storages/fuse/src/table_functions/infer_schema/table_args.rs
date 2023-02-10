//  Copyright 2023 Datafuse Labs.
//
//  Licensed under the Apache License, Version 2.0 (the "License");
//  you may not use this file except in compliance with the License.
//  You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
//  Unless required by applicable law or agreed to in writing, software
//  distributed under the License is distributed on an "AS IS" BASIS,
//  WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
//  See the License for the specific language governing permissions and
//  limitations under the License.

use common_exception::ErrorCode;
use common_exception::Result;
use common_storage::StageFilesInfo;

use crate::table_functions::string_value;
use crate::table_functions::TableArgs;

pub fn parse_infer_schema_args(table_args: &TableArgs) -> Result<(String, StageFilesInfo)> {
    let args = table_args.expect_all_named("infer_schema")?;

    let mut location = None;
    let mut files_info = StageFilesInfo {
        path: "".to_string(),
        files: None,
        pattern: None,
    };

    for (k, v) in &args {
        match k.to_lowercase().as_str() {
            "pattern" => {
                files_info.pattern = Some(string_value(v)?);
            }
            "location" => {
                location = Some(string_value(v)?);
            }
            _ => {
                return Err(ErrorCode::BadArguments(format!(
                    "unknown param {} for infer_schema",
                    k
                )));
            }
        }
    }

    let location = location.ok_or(ErrorCode::BadArguments(
        "infer_schema must specify location",
    ))?;

    Ok((location, files_info))
}
