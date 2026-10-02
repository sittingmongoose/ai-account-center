"""Fixed bounded read-only userinfo transport; imports never contact a provider."""
import json
import math
import ssl
import urllib.request

ENDPOINT = 'https://www.googleapis.com/oauth2/v2/userinfo'
MAX_RESPONSE = 16384


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, message, headers, newurl):
        return None


def request_userinfo(access_token, *, opener=None):
    if type(access_token) is not str or not access_token or len(access_token)>16384:
        raise ValueError('native-profile-unavailable')
    if opener is None:
        opener=urllib.request.build_opener(urllib.request.ProxyHandler({}),NoRedirect(),
            urllib.request.HTTPSHandler(context=ssl.create_default_context()))
    request=urllib.request.Request(ENDPOINT,headers={'Authorization':'Bearer '+access_token},method='GET')
    with opener.open(request,timeout=5) as response:
        raw=response.read(MAX_RESPONSE+1)
    if len(raw)>MAX_RESPONSE:raise ValueError('native-profile-unavailable')
    def unique(pairs):
        value={}
        for key,item in pairs:
            if key in value:raise ValueError('native-profile-unavailable')
            value[key]=item
        return value
    def finite(value):
        number=float(value)
        if not math.isfinite(number):raise ValueError('native-profile-unavailable')
        return number
    value=json.loads(raw,object_pairs_hook=unique,parse_float=finite,
        parse_constant=lambda _:(_ for _ in ()).throw(ValueError('native-profile-unavailable')))
    if type(value) is not dict:raise ValueError('native-profile-unavailable')
    return value
