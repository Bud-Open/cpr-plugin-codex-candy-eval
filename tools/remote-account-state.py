#!/usr/bin/env python3
"""目标侧账号状态探测；只输出白名单状态、计数与结构规模，不返回身份或凭据"""
import json
import math
import sys
from datetime import datetime

import remote_verify_run as verify

BOOLEAN_KEYS = {
    'enabled', 'disabled', 'available', 'unavailable', 'healthy',
    'rate_limited', 'expired', 'cooldown', 'penalty',
}
NUMBER_KEYS = {
    'cooldown', 'cooldown_ms', 'cooldown_seconds', 'penalty', 'penalty_ms',
    'quota', 'quota_remaining', 'remaining', 'remaining_quota',
    'models_count', 'model_count', 'in_flight', 'in_flight_count',
    'success_count', 'failure_count', 'request_count',
}
TIME_KEYS = {'cooldown_until', 'penalty_until', 'expires_at', 'reset_at', 'last_error_at'}
STATUS_VALUES = {
    'active', 'inactive', 'enabled', 'disabled', 'available', 'unavailable',
    'healthy', 'unhealthy', 'ready', 'running', 'stopped', 'pending',
    'cooldown', 'rate_limited', 'expired', 'error', 'invalid', 'frozen', 'unknown',
}
ENUM_VALUES = {
    'status': STATUS_VALUES,
    'state': STATUS_VALUES,
    'provider': {'openai', 'xai'},
    'provider_id': {'openai', 'xai'},
    'provider_kind': {'openai', 'xai'},
    'kind': {'openai', 'xai', 'oauth', 'api_key', 'token', 'bearer'},
    'type': {'openai', 'xai', 'oauth', 'api_key', 'token', 'bearer'},
    'authentication_kind': {'oauth', 'api_key', 'token', 'bearer'},
    'auth_kind': {'oauth', 'api_key', 'token', 'bearer'},
    'model_access': {'all', 'allowlist', 'denylist', 'restricted', 'unrestricted'},
    'model_access_mode': {'all', 'allowlist', 'denylist', 'restricted', 'unrestricted'},
    'plan': {'free', 'plus', 'pro', 'team', 'business', 'enterprise', 'edu', 'unknown'},
    'plan_type': {'free', 'plus', 'pro', 'team', 'business', 'enterprise', 'edu', 'unknown'},
}
SAFE_KEYS = BOOLEAN_KEYS | NUMBER_KEYS | TIME_KEYS | ENUM_VALUES.keys()
NESTED_KEYS = {'summary', 'quota', 'usage', 'limits', 'cooldown', 'penalty', 'runtime', 'health', 'model_access'}
LIST_KEYS = {'models', 'allowed_models', 'denied_models'}
KNOWN_KEYS = SAFE_KEYS | NESTED_KEYS | LIST_KEYS


def safe_scalar(value, key=''):
    # 长度短不能证明安全；类型、字段和枚举值必须同时符合合同
    if isinstance(value, bool):
        return value if key in BOOLEAN_KEYS else None
    if isinstance(value, (int, float)):
        return value if key in NUMBER_KEYS | TIME_KEYS and (not isinstance(value, float) or math.isfinite(value)) else None
    if isinstance(value, str):
        if value in ENUM_VALUES.get(key, set()):
            return value
        if key in TIME_KEYS:
            try:
                return datetime.fromisoformat(value.replace('Z', '+00:00')).isoformat()
            except ValueError:
                pass
    return None


def scrub(account):
    if not isinstance(account, dict):
        return {'object': False}
    out = {}
    for key in SAFE_KEYS & account.keys():
        value = safe_scalar(account[key], key)
        if value is not None:
            out[key] = value
    # 未知字段名也可能是动态账号标识，只统计数量，不回显原名
    out['_keys'] = sorted(KNOWN_KEYS & account.keys())
    out['_unknown_fields'] = len(account.keys() - KNOWN_KEYS)
    for key in NESTED_KEYS & account.keys():
        value = account[key]
        if not isinstance(value, dict):
            continue
        scalars = {k: safe_scalar(value[k], k) for k in SAFE_KEYS & value.keys()}
        out['_' + key] = {
            'keys': sorted(KNOWN_KEYS & value.keys()),
            'scalars': {k: v for k, v in scalars.items() if v is not None},
            'list_lengths': {k: len(value[k]) for k in LIST_KEYS & value.keys() if isinstance(value[k], list)},
            'unknown_fields': len(value.keys() - KNOWN_KEYS),
        }
    return out


def main():
    payload = verify.api('/api/admin/accounts')
    items = None
    for container in (payload, payload.get('data') if isinstance(payload, dict) else None):
        if isinstance(container, list):
            items = container
        elif isinstance(container, dict):
            for key in ('items', 'accounts', 'list', 'data'):
                if isinstance(container.get(key), list):
                    items = container[key]
                    break
        if items is not None:
            break
    if not isinstance(items, list):
        print(json.dumps({'phase': 'unexpected_shape', 'object': isinstance(payload, dict),
                          'top_field_count': len(payload) if isinstance(payload, dict) else None}))
        return 1
    print(json.dumps({
        'phase': 'ok',
        'count': len(items),
        'summary': scrub(payload.get('summary')) if isinstance(payload, dict) else None,
        'accounts': [scrub(item) for item in items],
    }, ensure_ascii=False))
    return 0


if __name__ == '__main__':
    try:
        sys.exit(main())
    except Exception as error:
        print(json.dumps({'phase': 'failed', 'error': type(error).__name__,
                          'detail': verify.redact(error)}), flush=True)
        sys.exit(1)
