#define NOMINMAX
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <d3d11_4.h>
#include <d3d11on12.h>
#include <d2d1_1.h>
#include <dxgi1_2.h>
#include <wrl/client.h>
#include <algorithm>
#include <array>
#include <chrono>
#include <cmath>
#include <cstdint>
#include <cstring>
#include <memory>
#include <set>
#include <span>
#ifdef FESTERM_D2D_PROBE
#include <psapi.h>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <sstream>
#endif
#include <map>
#include <stdexcept>
#include <string>
#include <vector>

using Microsoft::WRL::ComPtr;
using Clock = std::chrono::steady_clock;

namespace festerm_direct2d {

class GraphicsFailure : public std::runtime_error {
public:
    HRESULT code;
    explicit GraphicsFailure(HRESULT value)
        : std::runtime_error("DirectX HRESULT " + std::to_string(static_cast<uint32_t>(value))), code(value) {}
};

static void check(HRESULT result) {
    if (FAILED(result)) {
        throw GraphicsFailure(result);
    }
}

#ifdef FESTERM_D2D_PROBE
template<class T> static T read(std::istream& stream) {
    T value{};
    if (!stream.read(reinterpret_cast<char*>(&value), sizeof value))
        throw std::runtime_error("Truncated scene");
    return value;
}
static uint32_t count(std::istream& stream, uint32_t maximum) {
    const auto value = read<uint32_t>(stream);
    if (value > maximum) throw std::runtime_error("Scene count exceeds bound");
    return value;
}
#endif

static float raster_position(float value) {
    constexpr float scale = float(1 << D3D11_SUBPIXEL_FRACTIONAL_BIT_COUNT);
    return std::round(value * scale) / scale;
}

struct Color {
    uint8_t r, g, b, a;
    bool operator==(const Color&) const = default;
    D2D1_COLOR_F straight() const {
        const auto alpha = static_cast<float>(a);
        return {a ? r / alpha : 0, a ? g / alpha : 0, a ? b / alpha : 0, alpha / 255};
    }
};
struct Vertex { float x, y, u, v; Color color; };
static_assert(sizeof(Vertex) == 20 && sizeof(Color) == 4);

struct Texture {
    uint32_t width, height;
    bool mask;
    ComPtr<ID2D1Bitmap> bitmap;
};
struct Draw {
    enum Kind { Rectangle, Mask, Bitmap, Triangle } kind;
    D2D1_RECT_F destination{}, source{};
    Color color{};
    ComPtr<ID2D1Bitmap> bitmap;
    D2D1_MATRIX_3X2_F transform = D2D1::Matrix3x2F::Identity();
    ComPtr<ID2D1BitmapBrush> gradient;
    ComPtr<ID2D1BitmapBrush> fill;
};
struct Group { D2D1_RECT_F clip; std::vector<Draw> draws; };
using Frame = std::vector<Group>;

class Renderer {
public:
    ComPtr<ID3D11Device> device;
    ComPtr<ID3D12Device> native_device;
    ComPtr<ID3D11DeviceContext> immediate;
    ComPtr<ID3D11On12Device> on12;
    ComPtr<ID3D11Resource> wrapped;
    ComPtr<ID3D11DeviceContext4> context4;
    ComPtr<ID3D11Fence> fence;
    ComPtr<ID3D11Texture2D> target;
    ComPtr<ID2D1Factory1> factory;
    ComPtr<ID2D1Device> d2d;
    ComPtr<ID2D1DeviceContext> context;
    ComPtr<ID2D1Bitmap1> target_bitmap;
    ComPtr<ID2D1SolidColorBrush> brush;
    ComPtr<ID2D1PathGeometry> triangle_geometry;
    ComPtr<ID2D1Bitmap> alpha_ramp;
    std::map<uint8_t, ComPtr<ID2D1BitmapBrush>> gradients;
    std::map<uint32_t, ComPtr<ID2D1BitmapBrush>> color_brushes;
    std::set<uint32_t> used_colors;
    HANDLE event = nullptr;
    HANDLE timer = nullptr;
    uint64_t sequence = 0;
    size_t degenerate_triangles = 0;
    uint32_t width, height;
    DXGI_ADAPTER_DESC adapter{};

    Renderer(uint32_t w, uint32_t h, ID3D12Device* native = nullptr,
        ID3D12CommandQueue* queue = nullptr) : width(w), height(h) {
        if (native) {
            native_device = native;
            if (!queue || queue->GetDesc().Type != D3D12_COMMAND_LIST_TYPE_DIRECT)
                throw GraphicsFailure(E_INVALIDARG);
            ComPtr<ID3D12Device> queue_device;
            check(queue->GetDevice(IID_PPV_ARGS(&queue_device)));
            ComPtr<IUnknown> expected, actual;
            check(native->QueryInterface(IID_PPV_ARGS(&expected)));
            check(queue_device.As(&actual));
            if (expected.Get() != actual.Get()) throw GraphicsFailure(E_INVALIDARG);
            IUnknown* queues[]{queue};
            check(D3D11On12CreateDevice(native, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                nullptr, 0, queues, 1, 0, &device, &immediate, nullptr));
            check(device.As(&on12));
        } else {
            check(D3D11CreateDevice(nullptr, D3D_DRIVER_TYPE_WARP, nullptr,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT, nullptr, 0, D3D11_SDK_VERSION,
                &device, nullptr, &immediate));
        }
        ComPtr<IDXGIDevice> dxgi;
        check(device.As(&dxgi));
        ComPtr<IDXGIAdapter> native_adapter;
        check(dxgi->GetAdapter(&native_adapter));
        check(native_adapter->GetDesc(&adapter));
        D2D1_FACTORY_OPTIONS options{};
        check(D2D1CreateFactory(D2D1_FACTORY_TYPE_MULTI_THREADED, options, factory.GetAddressOf()));
        check(factory->CreateDevice(dxgi.Get(), &d2d));
        check(d2d->CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE, &context));
        if (!on12) {
            D3D11_TEXTURE2D_DESC description{};
            description.Width = width;
            description.Height = height;
            description.MipLevels = description.ArraySize = 1;
            description.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
            description.SampleDesc.Count = 1;
            description.BindFlags = D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE;
            check(device->CreateTexture2D(&description, nullptr, &target));
            bind_surface();
        }
        context->SetDpi(96, 96);
        context->SetAntialiasMode(D2D1_ANTIALIAS_MODE_ALIASED);
        check(context->CreateSolidColorBrush(D2D1::ColorF(0, 0, 0), &brush));
        check(factory->CreatePathGeometry(&triangle_geometry));
        ComPtr<ID2D1GeometrySink> sink;
        check(triangle_geometry->Open(&sink));
        sink->BeginFigure(D2D1::Point2F(0, 0), D2D1_FIGURE_BEGIN_FILLED);
        sink->AddLine(D2D1::Point2F(1, 0));
        sink->AddLine(D2D1::Point2F(0, 1));
        sink->EndFigure(D2D1_FIGURE_END_CLOSED);
        check(sink->Close());
        std::array<uint8_t, 256> ramp{};
        for (size_t i = 0; i < ramp.size(); ++i) ramp[i] = static_cast<uint8_t>(i);
        check(context->CreateBitmap(D2D1::SizeU(256, 1), ramp.data(), 256,
            D2D1::BitmapProperties(D2D1::PixelFormat(DXGI_FORMAT_A8_UNORM, D2D1_ALPHA_MODE_PREMULTIPLIED)),
            &alpha_ramp));
        if (!on12) {
            ComPtr<ID3D11Device5> device5;
            check(device.As(&device5));
            check(device5->CreateFence(0, D3D11_FENCE_FLAG_NONE, IID_PPV_ARGS(&fence)));
            check(immediate.As(&context4));
            event = CreateEventW(nullptr, FALSE, FALSE, nullptr);
            if (!event) throw std::runtime_error("CreateEvent failed");
            timer = CreateWaitableTimerExW(nullptr, nullptr, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS);
            if (!timer) throw std::runtime_error("CreateWaitableTimerEx failed");
        }
    }

    ~Renderer() {
        if (timer) CloseHandle(timer);
        if (event) CloseHandle(event);
    }

    void bind_surface() {
        ComPtr<IDXGISurface> surface;
        check(target.As(&surface));
        const auto properties = D2D1::BitmapProperties1(
            D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
            D2D1::PixelFormat(DXGI_FORMAT_B8G8R8A8_UNORM, D2D1_ALPHA_MODE_PREMULTIPLIED));
        check(context->CreateBitmapFromDxgiSurface(surface.Get(), properties, &target_bitmap));
    }

    void set_target(ID3D12Resource* resource, uint32_t w, uint32_t h) {
        context->SetTarget(nullptr);
        target_bitmap.Reset();
        target.Reset();
        wrapped.Reset();
        width = w; height = h;
        const D3D11_RESOURCE_FLAGS flags{D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE};
        check(on12->CreateWrappedResource(resource, &flags, D3D12_RESOURCE_STATE_RENDER_TARGET,
            D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE | D3D12_RESOURCE_STATE_NON_PIXEL_SHADER_RESOURCE,
            IID_PPV_ARGS(&wrapped)));
        check(wrapped.As(&target));
        bind_surface();
    }

    void pace(Clock::time_point deadline) {
        const auto remaining = std::chrono::duration_cast<std::chrono::nanoseconds>(deadline-Clock::now()).count();
        if (remaining <= 0) return;
        LARGE_INTEGER due{};
        due.QuadPart = -((remaining + 99) / 100);
        if (!SetWaitableTimer(timer, &due, 0, nullptr, nullptr, FALSE))
            throw std::runtime_error("SetWaitableTimer failed");
        if (WaitForSingleObject(timer, 1000) != WAIT_OBJECT_0)
            throw std::runtime_error("Pacing timer did not complete");
    }

    void complete() {
        check(context4->Signal(fence.Get(), ++sequence));
        immediate->Flush();
        check(fence->SetEventOnCompletion(sequence, event));
        if (WaitForSingleObject(event, 30000) != WAIT_OBJECT_0)
            throw std::runtime_error("GPU fence did not complete");
    }

    Texture texture(uint32_t w, uint32_t h, std::span<const Color> source) {
        if (!w || !h || w > 8192 || h > 8192 || uint64_t(w)*h > 16777216 ||
            source.size() != size_t(w)*h)
            throw std::runtime_error("Invalid atlas size");
        std::vector<Color> pixels(source.begin(), source.end());
        return texture_pixels(w, h, std::move(pixels));
    }

#ifdef FESTERM_D2D_PROBE
    Texture texture(std::istream& stream) {
        const auto w = count(stream, 8192), h = count(stream, 8192);
        if (!w || !h || uint64_t(w) * h > 16777216)
            throw std::runtime_error("Invalid atlas size");
        std::vector<Color> pixels(size_t(w) * h);
        if (!stream.read(reinterpret_cast<char*>(pixels.data()), pixels.size() * sizeof(Color)))
            throw std::runtime_error("Truncated atlas");
        return texture_pixels(w, h, std::move(pixels));
    }
#endif

    Texture texture_pixels(uint32_t w, uint32_t h, std::vector<Color> pixels) {
        const bool mask = std::all_of(pixels.begin(), pixels.end(), [](Color c) {
            return c.r == c.a && c.g == c.a && c.b == c.a;
        });
        Texture result{w, h, mask, {}};
        const auto properties = D2D1::BitmapProperties(
            D2D1::PixelFormat(mask ? DXGI_FORMAT_A8_UNORM : DXGI_FORMAT_B8G8R8A8_UNORM,
                D2D1_ALPHA_MODE_PREMULTIPLIED));
        if (mask) {
            std::vector<uint8_t> alpha;
            alpha.reserve(pixels.size());
            for (auto pixel : pixels) alpha.push_back(pixel.a);
            check(context->CreateBitmap(D2D1::SizeU(w, h), alpha.data(), w, properties, &result.bitmap));
        } else {
            for (auto& pixel : pixels) std::swap(pixel.r, pixel.b);
            check(context->CreateBitmap(D2D1::SizeU(w, h), pixels.data(), w * 4, properties, &result.bitmap));
        }
        return result;
    }

    Draw triangle(const std::array<Vertex, 3>& vertices) {
        Draw result{Draw::Triangle};
        const auto& p = vertices[0];
        const auto& q = vertices[1];
        const auto& r = vertices[2];
        result.transform = D2D1::Matrix3x2F(
            q.x-p.x, q.y-p.y, r.x-p.x, r.y-p.y, p.x, p.y);
        const auto strongest = std::max_element(vertices.begin(), vertices.end(),
            [](auto a, auto b) { return a.color.a < b.color.a; });
        result.color = strongest->color;
        if (vertices[0].color == vertices[1].color && vertices[1].color == vertices[2].color)
            return result;
        for (const auto& vertex : vertices) {
            if (vertex.color.a != 0 && !(vertex.color == result.color))
                throw std::runtime_error("Unsupported multi-color triangle");
        }
        const float ap = float(p.color.a) / result.color.a;
        const float aq = float(q.color.a) / result.color.a;
        const float ar = float(r.color.a) / result.color.a;
        const float x = aq-ap, y = ar-ap;
        const float length = x*x + y*y;
        if (length == 0) throw std::runtime_error("Invalid gradient");
        const D2D1_POINT_2F start{-ap*x/length, -ap*y/length};
        const D2D1_POINT_2F delta{x/length, y/length};
        const auto key = static_cast<uint8_t>((p.color.a ? 1 : 0) |
            (q.color.a ? 2 : 0) | (r.color.a ? 4 : 0));
        auto [entry, inserted] = gradients.try_emplace(key);
        if (inserted) {
            // Native gradient brushes filter a one-pixel ramp's endpoints (255 becomes 223).
            // A shared A8 bitmap preserves egui's interpolated vertex alpha without that filter.
            const auto transform = D2D1::Matrix3x2F(delta.x/255, delta.y/255, -delta.y, delta.x,
                start.x-delta.x/510, start.y-delta.y/510);
            check(context->CreateBitmapBrush(alpha_ramp.Get(),
                D2D1::BitmapBrushProperties(D2D1_EXTEND_MODE_CLAMP, D2D1_EXTEND_MODE_CLAMP,
                    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR),
                D2D1::BrushProperties(1, transform), &entry->second));
        }
        result.gradient = entry->second;
        const uint32_t color_key = result.color.r | uint32_t(result.color.g) << 8 |
            uint32_t(result.color.b) << 16 | uint32_t(result.color.a) << 24;
        used_colors.insert(color_key);
        if (!color_brushes.contains(color_key) && color_brushes.size() >= 256) {
            const auto retired = std::find_if(color_brushes.begin(), color_brushes.end(),
                [&](const auto& entry) { return !used_colors.contains(entry.first); });
            if (retired == color_brushes.end())
                throw std::runtime_error("A frame supports at most 256 feathered colors");
            color_brushes.erase(retired);
        }
        auto [color_entry, new_color] = color_brushes.try_emplace(color_key);
        if (new_color) {
            // FillGeometry requires a clamped bitmap brush when an opacity brush is present.
            const uint8_t pixel[]{result.color.b, result.color.g, result.color.r, result.color.a};
            ComPtr<ID2D1Bitmap> bitmap;
            check(context->CreateBitmap(D2D1::SizeU(1, 1), pixel, 4,
                D2D1::BitmapProperties(D2D1::PixelFormat(DXGI_FORMAT_B8G8R8A8_UNORM,
                    D2D1_ALPHA_MODE_PREMULTIPLIED)), &bitmap));
            check(context->CreateBitmapBrush(bitmap.Get(),
                D2D1::BitmapBrushProperties(D2D1_EXTEND_MODE_CLAMP, D2D1_EXTEND_MODE_CLAMP),
                &color_entry->second));
        }
        result.fill = color_entry->second;
        return result;
    }

    void draw(const Frame& frame, Color clear = {0,0,0,0}) {
        if (on12) {
            ID3D11Resource* resources[]{wrapped.Get()};
            on12->AcquireWrappedResources(resources, 1);
        }
        context->SetTarget(target_bitmap.Get());
        context->BeginDraw();
        context->Clear(clear.straight());
        for (const auto& group : frame) {
            context->PushAxisAlignedClip(group.clip, D2D1_ANTIALIAS_MODE_ALIASED);
            for (const auto& item : group.draws) {
                brush->SetColor(item.color.straight());
                switch (item.kind) {
                case Draw::Rectangle:
                    context->FillRectangle(item.destination, brush.Get());
                    break;
                case Draw::Mask:
                    context->FillOpacityMask(item.bitmap.Get(), brush.Get(), &item.destination, &item.source);
                    break;
                case Draw::Bitmap:
                    context->DrawBitmap(item.bitmap.Get(), item.destination, float(item.color.a)/255,
                        D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, item.source);
                    break;
                case Draw::Triangle:
                    context->SetTransform(item.transform);
                    context->FillGeometry(triangle_geometry.Get(), item.fill ?
                        static_cast<ID2D1Brush*>(item.fill.Get()) : brush.Get(), item.gradient.Get());
                    context->SetTransform(D2D1::Matrix3x2F::Identity());
                    break;
                }
            }
            context->PopAxisAlignedClip();
        }
        const auto result = context->EndDraw();
        if (on12) {
            ID3D11Resource* resources[]{wrapped.Get()};
            on12->ReleaseWrappedResources(resources, 1);
            immediate->Flush();
            context->SetTarget(nullptr);
            target_bitmap.Reset();
            target.Reset();
            wrapped.Reset();
        } else {
            complete();
        }
        check(result);
    }

#ifdef FESTERM_D2D_PROBE
    void capture(const std::filesystem::path& path) {
        D3D11_TEXTURE2D_DESC description{};
        target->GetDesc(&description);
        description.Usage = D3D11_USAGE_STAGING;
        description.BindFlags = 0;
        description.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
        ComPtr<ID3D11Texture2D> staging;
        check(device->CreateTexture2D(&description, nullptr, &staging));
        immediate->CopyResource(staging.Get(), target.Get());
        complete();
        D3D11_MAPPED_SUBRESOURCE mapped{};
        check(immediate->Map(staging.Get(), 0, D3D11_MAP_READ, 0, &mapped));
        std::ofstream output(path, std::ios::binary);
        for (uint32_t y = 0; y < height; ++y) {
            const auto row = static_cast<const char*>(mapped.pData) + size_t(y)*mapped.RowPitch;
            output.write(row, width * 4);
        }
        immediate->Unmap(staging.Get(), 0);
        if (!output) throw std::runtime_error("Readback write failed");
    }
#endif
};

static bool quad(const std::array<Vertex, 6>& vertices, Draw& result, const Texture& texture) {
    auto left = vertices[0].x, right = left, top = vertices[0].y, bottom = top;
    for (const auto& v : vertices) {
        left = std::min(left, v.x); right = std::max(right, v.x);
        top = std::min(top, v.y); bottom = std::max(bottom, v.y);
        if (!(v.color == vertices[0].color)) return false;
    }
    if (right <= left || bottom <= top) return false;
    std::array<const Vertex*, 4> corners{};
    for (const auto& v : vertices) {
        if ((v.x != left && v.x != right) || (v.y != top && v.y != bottom)) return false;
        const int index = (v.x == right ? 1 : 0) + (v.y == bottom ? 2 : 0);
        if (corners[index] && (corners[index]->u != v.u || corners[index]->v != v.v)) return false;
        corners[index] = &v;
    }
    for (auto corner : corners) if (!corner) return false;
    std::vector<Vertex> shared;
    for (size_t i = 0; i < 3; ++i)
        for (size_t j = 3; j < 6; ++j)
            if (vertices[i].x == vertices[j].x && vertices[i].y == vertices[j].y)
                shared.push_back(vertices[i]);
    if (shared.size() != 2 || shared[0].x == shared[1].x || shared[0].y == shared[1].y)
        return false;
    const auto area = [](Vertex a, Vertex b, Vertex c) {
        return std::abs((b.x-a.x)*(c.y-a.y)-(c.x-a.x)*(b.y-a.y))/2;
    };
    if (std::abs(area(vertices[0],vertices[1],vertices[2]) +
        area(vertices[3],vertices[4],vertices[5]) - (right-left)*(bottom-top)) > 0.01f)
        return false;
    result.destination = {left, top, right, bottom};
    result.color = vertices[0].color;
    if (std::all_of(vertices.begin(), vertices.end(), [](auto v) { return v.u == 0 && v.v == 0; })) {
        result.kind = Draw::Rectangle;
        return true;
    }
    if (corners[0]->u != corners[2]->u || corners[1]->u != corners[3]->u ||
        corners[0]->v != corners[1]->v || corners[2]->v != corners[3]->v ||
        corners[1]->u <= corners[0]->u || corners[2]->v <= corners[0]->v)
        return false;
    result.source = {corners[0]->u*texture.width, corners[0]->v*texture.height,
        corners[3]->u*texture.width, corners[3]->v*texture.height};
    result.kind = texture.mask ? Draw::Mask : Draw::Bitmap;
    result.bitmap = texture.bitmap;
    if (!texture.mask && (result.color.r != result.color.a ||
        result.color.g != result.color.a || result.color.b != result.color.a))
        throw std::runtime_error("Unsupported tinted color bitmap");
    return true;
}

static Group prepare_group(Renderer& renderer, const Texture& texture, D2D1_RECT_F clip,
    std::span<const Vertex> source, std::span<const uint32_t> indices) {
    if (source.size() > 1000000 || indices.size() > 3000000 || indices.size() % 3)
        throw std::runtime_error("Invalid mesh size");
    Group group{};
    group.clip = {
        std::clamp(std::round(clip.left), 0.0f, float(renderer.width)),
        std::clamp(std::round(clip.top), 0.0f, float(renderer.height)),
        std::clamp(std::round(clip.right), 0.0f, float(renderer.width)),
        std::clamp(std::round(clip.bottom), 0.0f, float(renderer.height))};
    if (group.clip.right <= group.clip.left || group.clip.bottom <= group.clip.top)
        return group;
    std::vector<Vertex> vertices(source.begin(), source.end());
    for (auto& vertex : vertices) {
        if (!std::isfinite(vertex.x) || !std::isfinite(vertex.y) ||
            !std::isfinite(vertex.u) || !std::isfinite(vertex.v) ||
            std::abs(vertex.x) > 1000000 || std::abs(vertex.y) > 1000000)
            throw std::runtime_error("Invalid vertex coordinates");
        if (vertex.u < 0 || vertex.u > 1 || vertex.v < 0 || vertex.v > 1 ||
            vertex.color.r > vertex.color.a || vertex.color.g > vertex.color.a ||
            vertex.color.b > vertex.color.a)
            throw std::runtime_error("Unsupported texture coordinates or additive color");
        vertex.x = raster_position(vertex.x);
        vertex.y = raster_position(vertex.y);
    }
    for (auto index : indices)
        if (index >= vertices.size()) throw std::runtime_error("Invalid mesh index");
    for (size_t i = 0; i < indices.size();) {
        Draw item{};
        if (i + 6 <= indices.size() && quad({
            vertices[indices[i]], vertices[indices[i+1]], vertices[indices[i+2]],
            vertices[indices[i+3]], vertices[indices[i+4]], vertices[indices[i+5]]
            }, item, texture)) {
            group.draws.push_back(std::move(item));
            i += 6;
        } else {
            std::array<Vertex, 3> triangle{vertices[indices[i]],
                vertices[indices[i+1]], vertices[indices[i+2]]};
            for (auto v : triangle) if (v.u != 0 || v.v != 0)
                throw std::runtime_error("Unsupported nonrectangular textured mesh");
            const auto& a = triangle[0];
            const auto& b = triangle[1];
            const auto& c = triangle[2];
            if ((b.x-a.x)*(c.y-a.y) == (c.x-a.x)*(b.y-a.y)) {
                ++renderer.degenerate_triangles;
            } else {
                group.draws.push_back(renderer.triangle(triangle));
            }
            i += 3;
        }
    }
    return group;
}

struct MeshInput {
    uint64_t texture;
    const Vertex* vertices;
    size_t vertex_count;
    const uint32_t* indices;
    size_t index_count;
    D2D1_RECT_F clip;
};

struct Bridge {
    std::unique_ptr<Renderer> renderer;
    std::map<uint64_t, Texture> textures;
    Frame frame;
    Color clear{};
    char error[512]{};
};

template<class Function> static HRESULT boundary(Bridge* bridge, Function&& function) {
    const auto message = [&](const char* text) {
        if (bridge) strncpy_s(bridge->error, text, _TRUNCATE);
    };
    try {
        if (bridge) bridge->error[0] = '\0';
        function();
        return S_OK;
    } catch (const GraphicsFailure& error) {
        message(error.what());
        return error.code;
    } catch (const std::bad_alloc&) {
        message("Direct2D allocation failed");
        return E_OUTOFMEMORY;
    } catch (const std::runtime_error& error) {
        message(error.what());
        return E_NOTIMPL;
    } catch (const std::exception& error) {
        // No C++ exception may cross the Rust ABI boundary.
        message(error.what());
        return E_FAIL;
    }
}

extern "C" HRESULT festerm_d2d_create(void* device, void* queue, Bridge** output) {
    if (!device || !queue || !output) return E_INVALIDARG;
    *output = nullptr;
    return boundary(nullptr, [&] {
        auto bridge = std::make_unique<Bridge>();
        bridge->renderer = std::make_unique<Renderer>(0, 0,
            static_cast<ID3D12Device*>(device), static_cast<ID3D12CommandQueue*>(queue));
        *output = bridge.release();
    });
}

extern "C" void festerm_d2d_destroy(Bridge* bridge) {
    delete bridge;
}

extern "C" const char* festerm_d2d_error(const Bridge* bridge) {
    return bridge ? bridge->error : "";
}

extern "C" HRESULT festerm_d2d_texture(Bridge* bridge, uint64_t id, uint32_t w, uint32_t h,
    const Color* pixels, size_t pixel_count) {
    if (!bridge || !pixels) return E_INVALIDARG;
    return boundary(bridge, [&] {
        bridge->textures.insert_or_assign(id,
            bridge->renderer->texture(w, h, std::span(pixels, pixel_count)));
    });
}

extern "C" HRESULT festerm_d2d_prune(Bridge* bridge, const uint64_t* ids, size_t count) {
    if (!bridge || (!ids && count)) return E_INVALIDARG;
    return boundary(bridge, [&] {
        const std::set<uint64_t> retained(ids, ids + count);
        std::erase_if(bridge->textures, [&](const auto& entry) { return !retained.contains(entry.first); });
    });
}

extern "C" HRESULT festerm_d2d_prepare(Bridge* bridge, uint32_t width, uint32_t height,
    Color clear, const MeshInput* meshes, size_t count) {
    if (!bridge || (!meshes && count) || !width || !height ||
        width > 8192 || height > 8192 || count > 250000) return E_INVALIDARG;
    return boundary(bridge, [&] {
        auto& renderer = *bridge->renderer;
        renderer.width = width;
        renderer.height = height;
        renderer.used_colors.clear();
        Frame frame;
        frame.reserve(count);
        for (const auto& mesh : std::span(meshes, count)) {
            if ((!mesh.vertices && mesh.vertex_count) || (!mesh.indices && mesh.index_count))
                throw std::runtime_error("Invalid mesh storage");
            const auto texture = bridge->textures.find(mesh.texture);
            if (texture == bridge->textures.end()) throw std::runtime_error("Missing texture pixels");
            frame.push_back(prepare_group(renderer, texture->second, mesh.clip,
                std::span(mesh.vertices, mesh.vertex_count), std::span(mesh.indices, mesh.index_count)));
        }
        bridge->frame = std::move(frame);
        bridge->clear = clear;
    });
}

extern "C" HRESULT festerm_d2d_draw(Bridge* bridge, ID3D12Resource** output) {
    if (!bridge || !output) return E_INVALIDARG;
    *output = nullptr;
    return boundary(bridge, [&] {
        auto& renderer = *bridge->renderer;
        D3D12_HEAP_PROPERTIES heap{};
        heap.Type = D3D12_HEAP_TYPE_DEFAULT;
        heap.CreationNodeMask = heap.VisibleNodeMask = 1;
        D3D12_RESOURCE_DESC description{};
        description.Dimension = D3D12_RESOURCE_DIMENSION_TEXTURE2D;
        description.Width = renderer.width;
        description.Height = renderer.height;
        description.DepthOrArraySize = description.MipLevels = 1;
        description.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
        description.SampleDesc.Count = 1;
        description.Flags = D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET;
        ComPtr<ID3D12Resource> resource;
        // A committed allocation follows COM/D3D11 deferred lifetime, even if
        // wgpu loses its device before recording the external work's fence.
        check(renderer.native_device->CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE,
            &description, D3D12_RESOURCE_STATE_RENDER_TARGET, nullptr, IID_PPV_ARGS(&resource)));
        renderer.set_target(resource.Get(), renderer.width, renderer.height);
        renderer.draw(bridge->frame, bridge->clear);
        *output = resource.Detach();
    });
}

#ifdef FESTERM_D2D_PROBE
static std::vector<Frame> frames(std::istream& stream, Renderer& renderer,
    const std::map<uint32_t, Texture>& textures) {
    std::vector<Frame> result(count(stream, 4));
    if (result.empty()) throw std::runtime_error("No frames");
    for (auto& frame : result) {
        frame.resize(count(stream, 100000));
        for (auto& group : frame) {
            group.clip = read<D2D1_RECT_F>(stream);
            group.clip = {
                std::clamp(std::round(group.clip.left), 0.0f, float(renderer.width)),
                std::clamp(std::round(group.clip.top), 0.0f, float(renderer.height)),
                std::clamp(std::round(group.clip.right), 0.0f, float(renderer.width)),
                std::clamp(std::round(group.clip.bottom), 0.0f, float(renderer.height))};
            const auto kind = read<uint32_t>(stream);
            if (kind == 0) {
                Draw item{Draw::Rectangle};
                item.destination = read<D2D1_RECT_F>(stream);
                item.color = read<Color>(stream);
                group.draws.push_back(std::move(item));
            } else if (kind == 1) {
                const auto& texture = textures.at(read<uint32_t>(stream));
                std::vector<Vertex> vertices(count(stream, 1000000));
                std::vector<uint32_t> indices(count(stream, 3000000));
                for (auto& vertex : vertices) vertex = read<Vertex>(stream);
                for (auto& index : indices) index = read<uint32_t>(stream);
                group = prepare_group(renderer, texture, group.clip, vertices, indices);
            } else throw std::runtime_error("Unknown primitive");
        }
    }
    if (stream.peek() != std::char_traits<char>::eof()) throw std::runtime_error("Trailing scene data");
    return result;
}

static double cpu_ms() {
    FILETIME created, exited, kernel, user;
    if (!GetProcessTimes(GetCurrentProcess(), &created, &exited, &kernel, &user))
        throw std::runtime_error("GetProcessTimes failed");
    const auto ticks = [](FILETIME value) {
        return (uint64_t(value.dwHighDateTime) << 32) | value.dwLowDateTime;
    };
    return double(ticks(kernel) + ticks(user)) / 10000;
}

static void self_test() {
    if (raster_position(1314.0001220703125f) != 1314.0f ||
        raster_position(955.703125f) != 955.703125f)
        throw std::runtime_error("Raster-grid normalization failed");
    const Color white{255,255,255,255};
    const Vertex a{0,0,0,0,white}, b{10,0,1,0,white}, c{10,20,1,1,white}, d{0,20,0,1,white};
    const Texture texture{64,64,true,{}};
    Draw draw{};
    if (!quad({a,b,c,a,c,d}, draw, texture) || draw.kind != Draw::Mask ||
        draw.source.right != 64 || draw.destination.bottom != 20)
        throw std::runtime_error("Quad mapping test failed");
    if (quad({a,b,c,a,b,d}, draw, texture))
        throw std::runtime_error("Overlapping triangles accepted as a quad");
    auto changed = c;
    changed.color = {128,128,128,128};
    if (quad({a,b,changed,a,changed,d}, draw, texture))
        throw std::runtime_error("Varying vertex colors accepted as a quad");
    auto reversed = b;
    reversed.u = -1;
    if (quad({a,reversed,c,a,c,d}, draw, texture))
        throw std::runtime_error("Invalid texture mapping accepted");
    std::istringstream truncated(std::string(1, '\0'));
    bool rejected = false;
    try { read<uint32_t>(truncated); } catch (const std::runtime_error&) { rejected = true; }
    if (!rejected) throw std::runtime_error("Truncated input accepted");
    std::istringstream oversized(std::string(4, '\xff'));
    rejected = false;
    try { count(oversized, 128); } catch (const std::runtime_error&) { rejected = true; }
    if (!rejected) throw std::runtime_error("Unbounded allocation accepted");
    std::cout << "7 native probe checks passed\n";
}

} // namespace festerm_direct2d

int wmain(int argc, wchar_t** argv) {
    using namespace festerm_direct2d;
    try {
        if (argc == 2 && std::wstring(argv[1]) == L"--self-test") {
            self_test();
            return 0;
        }
        if (argc != 4) throw std::runtime_error("Usage: render_probe scene output-json sample-ms");
        const std::filesystem::path path(argv[1]), output_path(argv[2]);
        const auto milliseconds = std::stoul(argv[3]);
        if (milliseconds != 0 && (milliseconds < 500 || milliseconds > 30000))
            throw std::runtime_error("Invalid sample duration");
        if (std::filesystem::file_size(path) > 256 * 1024 * 1024)
            throw std::runtime_error("Scene exceeds file limit");
        std::ifstream input(path, std::ios::binary);
        std::array<char, 8> magic{};
        input.read(magic.data(), magic.size());
        if (magic != std::array<char,8>{'F','E','S','D','2','D','0','1'})
            throw std::runtime_error("Invalid scene version");
        const auto width = count(input, 4096), height = count(input, 4096);
        if (!width || !height) throw std::runtime_error("Empty render target");
        Renderer renderer(width, height);
        std::map<uint32_t, Texture> textures;
        const auto texture_count = count(input, 128);
        for (uint32_t i = 0; i < texture_count; ++i) {
            const auto id = read<uint32_t>(input);
            if (!textures.emplace(id, renderer.texture(input)).second)
                throw std::runtime_error("Duplicate texture");
        }
        auto replay = frames(input, renderer, textures);
        if (milliseconds == 0) {
            renderer.draw(replay[0]);
            auto capture_path = output_path;
            capture_path.replace_extension(".bgra");
            renderer.capture(capture_path);
            std::ofstream output(output_path);
            output << "{\"mode\":\"capture-only\"}\n";
            if (!output) throw std::runtime_error("Results write failed");
            return 0;
        }
        for (size_t i = 0; i < 4; ++i) renderer.draw(replay[i % replay.size()]);
        const auto before_cpu = cpu_ms();
        const auto start = Clock::now();
        std::vector<double> timings;
        while (Clock::now() - start < std::chrono::milliseconds(milliseconds)) {
            const auto frame_start = Clock::now();
            renderer.draw(replay[timings.size() % replay.size()]);
            timings.push_back(std::chrono::duration<double, std::milli>(Clock::now()-frame_start).count());
            renderer.pace(frame_start + std::chrono::milliseconds(100));
        }
        const auto wall = std::chrono::duration<double, std::milli>(Clock::now()-start).count();
        const auto cpu = cpu_ms() - before_cpu;
        if (timings.size() < 2) throw std::runtime_error("Insufficient completed frames");
        PROCESS_MEMORY_COUNTERS_EX memory{};
        if (!GetProcessMemoryInfo(GetCurrentProcess(), reinterpret_cast<PROCESS_MEMORY_COUNTERS*>(&memory), sizeof memory))
            throw std::runtime_error("GetProcessMemoryInfo failed");
        std::sort(timings.begin(), timings.end());
        renderer.draw(replay[0]);
        auto capture_path = output_path; capture_path.replace_extension(".bgra");
        renderer.capture(capture_path);
        std::ofstream output(output_path);
        output << "{\"renderer\":\"Direct2D-D3D11-WARP\","
            << "\"adapter_vendor\":" << renderer.adapter.VendorId
            << ",\"adapter_device\":" << renderer.adapter.DeviceId
            << ",\"width\":" << width << ",\"height\":" << height
            << ",\"frames\":" << timings.size() << ",\"wall_ms\":" << wall
            << ",\"cpu_ms\":" << cpu << ",\"cpu_percent\":"
            << cpu/wall/GetActiveProcessorCount(ALL_PROCESSOR_GROUPS)*100
            << ",\"target_hz\":10,\"completed_hz\":" << timings.size()*1000.0/wall
            << ",\"cpu_ms_per_frame\":" << cpu/timings.size()
            << ",\"mean_ms\":" << [&] { double sum = 0; for (auto time : timings) sum += time; return sum/timings.size(); }()
            << ",\"median_ms\":" << timings[timings.size()/2]
            << ",\"p95_ms\":" << timings[timings.size()*95/100]
            << ",\"working_set\":" << memory.WorkingSetSize
            << ",\"private_bytes\":" << memory.PrivateUsage
            << ",\"gradient_brushes\":" << renderer.gradients.size()
            << ",\"color_brushes\":" << renderer.color_brushes.size()
            << ",\"degenerate_triangles\":" << renderer.degenerate_triangles << "}\n";
        if (!output) throw std::runtime_error("Results write failed");
        std::wcout << L"Completed " << path << L": " << renderer.adapter.Description << L"\n";
        return 0;
    } catch (const std::exception& error) {
        std::cerr << error.what() << "\n";
        return 1;
    }
}
#else
} // namespace festerm_direct2d
#endif
